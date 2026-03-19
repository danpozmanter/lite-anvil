use mlua::prelude::*;

const BOOTSTRAP: &str = r#"
local core = require "core"
local command = require "core.command"
local common = require "core.common"
local config = require "core.config"
local keymap = require "core.keymap"
local style = require "core.style"
local View = require "core.view"
local native = require "terminal_view_native"

local TerminalView = View:extend()

function TerminalView:__tostring() return "TerminalView" end

TerminalView.context = "session"

function TerminalView:new(options)
  TerminalView.super.new(self)
  native.init(self, options or {})
end

function TerminalView:get_name()
  local suffix = self.handle and self.handle:running() and "" or " [done]"
  return self.title .. suffix
end

function TerminalView:get_line_height()
  return math.floor(self.font:get_height() * config.line_height)
end

function TerminalView:get_char_width()
  return self.font:get_width("M")
end

function TerminalView:get_line_text_y_offset()
  local lh = self:get_line_height()
  local th = self.font:get_height()
  return (lh - th) / 2
end

function TerminalView:get_content_size()
  return self.cols * self:get_char_width(), self.rows * self:get_line_height()
end

function TerminalView:get_scrollable_size()
  return (self.buffer:total_rows()) * self:get_line_height() + style.padding.y * 2
end

function TerminalView:supports_text_input()
  return true
end

function TerminalView:resize_screen(cols, rows)
  native.resize_screen(self, cols, rows)
end

function TerminalView:spawn(command_argv)
  native.spawn(self, command_argv)
end

function TerminalView:try_close(do_close)
  if self.handle and self.handle:running() then
    core.nag_view:show(
      "Close Terminal",
      "This terminal is still running. Terminate it and close the tab?",
      {
        { text = "Terminate", default_yes = true },
        { text = "Cancel", default_no = true },
      },
      function(item)
        if item.text == "Terminate" then
          pcall(function() self.handle:terminate() end)
          do_close()
        end
      end
    )
    return
  end
  do_close()
end

function TerminalView:send_input(text)
  if self.handle and self.handle:running() then
    core.blink_reset()
    self.handle:write(text)
  end
end

function TerminalView:on_text_input(text)
  self:send_input(text)
end

function TerminalView:on_mouse_wheel(dy, dx)
  self.scroll.to.y = math.max(0, self.scroll.to.y - dy * self:get_line_height() * 3)
  return true
end

function TerminalView:clear()
  self.buffer:clear()
  self:resize_screen(self.cols, self.rows)
  native.scroll_to_bottom(self, true)
end

function TerminalView:scroll_to_bottom(force)
  native.scroll_to_bottom(self, force)
end

function TerminalView:get_dimensions()
  return native.get_dimensions(self)
end

function TerminalView:update()
  TerminalView.super.update(self)
  native.update(self)
end

function TerminalView:draw_row(row, x, y)
  local cell_w = self:get_char_width()
  local cell_h = self:get_line_height()
  for _, run in ipairs(row.runs or {}) do
    if run.bg then
      renderer.draw_rect(
        x + (run.start_col - 1) * cell_w,
        y,
        (run.end_col - run.start_col + 1) * cell_w,
        cell_h,
        run.bg
      )
    end
  end
  for _, run in ipairs(row.runs or {}) do
    renderer.draw_text(
      self.font,
      run.text,
      x + (run.start_col - 1) * cell_w,
      y + self:get_line_text_y_offset(),
      run.fg or self.default_fg
    )
  end
end

function TerminalView:draw_cursor()
  native.draw_cursor(self)
end

function TerminalView:draw()
  self:draw_background(style.background)
  renderer.draw_rect(self.position.x, self.position.y, self.size.x, self.size.y, self.default_bg)

  local total_rows = self.buffer:total_rows()
  local first_row = math.max(1, math.floor(self.scroll.y / self:get_line_height()) + 1)
  local last_row = math.min(total_rows, math.ceil((self.scroll.y + self.size.y) / self:get_line_height()) + 1)
  local x = self.position.x + style.padding.x

  core.push_clip_rect(self.position.x, self.position.y, self.size.x, self.size.y)
  for _, row in ipairs(self.buffer:render_rows(first_row, last_row)) do
    local y = self.position.y + style.padding.y + (row.index - 1) * self:get_line_height() - self.scroll.y
    self:draw_row(row, x, y)
  end
  self:draw_cursor()
  core.pop_clip_rect()

  self:draw_scrollbar()
end

function TerminalView.open(cwd, command_argv, title, placement)
  return native.open(TerminalView, cwd, command_argv, title, placement)
end

function TerminalView:rename()
  core.command_view:enter("Rename Terminal", {
    text = self.title:gsub("^Terminal:%s*", ""),
    submit = function(text)
      if text == "" then
        return
      end
      self.title = text
      core.redraw = true
    end,
  })
end

function TerminalView:switch_color_scheme(direction)
  native.switch_color_scheme(self, direction)
end

command.add(TerminalView, {
  ["terminal:send-enter"] = function(view) view:send_input("\r") end,
  ["terminal:send-backspace"] = function(view) view:send_input(string.char(0x7f)) end,
  ["terminal:send-tab"] = function(view) view:send_input("\t") end,
  ["terminal:send-escape"] = function(view) view:send_input("\27") end,
  ["terminal:send-up"] = function(view) view:send_input("\27[A") end,
  ["terminal:send-down"] = function(view) view:send_input("\27[B") end,
  ["terminal:send-right"] = function(view) view:send_input("\27[C") end,
  ["terminal:send-left"] = function(view) view:send_input("\27[D") end,
  ["terminal:send-home"] = function(view) view:send_input("\27[H") end,
  ["terminal:send-end"] = function(view) view:send_input("\27[F") end,
  ["terminal:interrupt"] = function(view) view:send_input(string.char(3)) end,
  ["terminal:send-eof"] = function(view) view:send_input(string.char(4)) end,
  ["terminal:suspend"] = function(view) view:send_input(string.char(26)) end,
  ["terminal:clear"] = function(view)
    view:clear()
    view:send_input(string.char(12))
  end,
  ["terminal:rename"] = function(view) view:rename() end,
  ["terminal:next-colorscheme"] = function(view) view:switch_color_scheme(1) end,
  ["terminal:previous-colorscheme"] = function(view) view:switch_color_scheme(-1) end,
})

keymap.add {
  ["return"] = "terminal:send-enter",
  ["backspace"] = "terminal:send-backspace",
  ["tab"] = "terminal:send-tab",
  ["escape"] = "terminal:send-escape",
  ["up"] = "terminal:send-up",
  ["down"] = "terminal:send-down",
  ["left"] = "terminal:send-left",
  ["right"] = "terminal:send-right",
  ["home"] = "terminal:send-home",
  ["end"] = "terminal:send-end",
  ["ctrl+c"] = "terminal:interrupt",
  ["ctrl+d"] = "terminal:send-eof",
  ["ctrl+l"] = "terminal:clear",
  ["ctrl+z"] = "terminal:suspend",
  ["ctrl+alt+s"] = "terminal:rename",
  ["ctrl+alt+]"] = "terminal:next-colorscheme",
  ["ctrl+alt+["] = "terminal:previous-colorscheme",
}

return TerminalView
"#;

fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
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

fn normalize_path(lua: &Lua, path: &str) -> LuaResult<String> {
    let common = require_table(lua, "core.common")?;
    let normalize: LuaFunction = common.get("normalize_path")?;
    normalize.call(path.to_string())
}

fn default_cwd(lua: &Lua) -> LuaResult<String> {
    let core = require_table(lua, "core")?;
    let common = require_table(lua, "core.common")?;
    let dirname: LuaFunction = common.get("dirname")?;
    let active_view: LuaValue = core.get("active_view")?;
    if let LuaValue::Table(view) = active_view {
        if let Some(doc) = view.get::<Option<LuaTable>>("doc")? {
            if let Some(abs_filename) = doc.get::<Option<String>>("abs_filename")? {
                let dir: String = dirname.call(abs_filename)?;
                return normalize_path(lua, &dir);
            }
        }
    }
    if let Some(root_project) = core.get::<Option<LuaFunction>>("root_project")? {
        if let Some(project) = root_project.call::<Option<LuaTable>>(())? {
            if let Some(path) = project.get::<Option<String>>("path")? {
                return normalize_path(lua, &path);
            }
        }
    }
    normalize_path(lua, &std::env::var("HOME").unwrap_or_else(|_| ".".to_string()))
}

fn hex_to_rgba(lua: &Lua, hex: String) -> LuaResult<LuaTable> {
    let common = require_table(lua, "core.common")?;
    let color: LuaFunction = common.get("color")?;
    let (r, g, b, a): (i64, i64, i64, i64) = color.call(hex)?;
    let out = lua.create_table()?;
    out.raw_set(1, r)?;
    out.raw_set(2, g)?;
    out.raw_set(3, b)?;
    out.raw_set(4, a)?;
    Ok(out)
}

fn table_color(lua: &Lua, src: LuaTable) -> LuaResult<LuaTable> {
    let out = lua.create_table()?;
    out.raw_set(1, src.raw_get::<i64>(1)?)?;
    out.raw_set(2, src.raw_get::<i64>(2)?)?;
    out.raw_set(3, src.raw_get::<i64>(3)?)?;
    let alpha = src.raw_get::<Option<i64>>(4)?.unwrap_or(0xff);
    out.raw_set(4, alpha)?;
    Ok(out)
}

fn make_palette(lua: &Lua, name: Option<String>) -> LuaResult<(LuaTable, LuaTable)> {
    let config = require_table(lua, "core.config")?;
    let plugins: LuaTable = config.get("plugins")?;
    let terminal_cfg: LuaTable = plugins.get("terminal")?;
    let schemes = require_table(lua, "plugins.terminal.colors")?;
    let configured = terminal_cfg.get::<Option<String>>("color_scheme")?;
    let requested = name.or(configured).unwrap_or_else(|| "eterm".to_string());
    let scheme = schemes
        .get::<Option<LuaTable>>(requested.clone())?
        .or_else(|| {
            schemes
                .get::<Option<LuaTable>>("eterm")
                .ok()
                .flatten()
        })
        .ok_or_else(|| LuaError::RuntimeError("missing terminal color schemes".into()))?;
    let out = lua.create_table()?;
    if let Some(palette) = scheme.get::<Option<LuaTable>>("palette")? {
        let mut idx = 1i64;
        for value in palette.sequence_values::<String>() {
            out.raw_set(idx, hex_to_rgba(lua, value?)?)?;
            idx += 1;
        }
    }
    Ok((scheme, out))
}

fn palette_table(lua: &Lua, view: &LuaTable) -> LuaResult<LuaTable> {
    let palette: LuaTable = view.get("palette")?;
    let out = lua.create_table()?;
    for i in 1..=16 {
        out.raw_set(i as i64, palette.raw_get::<LuaValue>(i as i64)?)?;
    }
    Ok(out)
}

fn apply_color_scheme(lua: &Lua, view: &LuaTable, name: Option<String>) -> LuaResult<()> {
    let style = require_table(lua, "core.style")?;
    let (scheme, palette) = make_palette(lua, name.clone())?;
    let chosen = name
        .or_else(|| scheme.get::<Option<String>>("name").ok().flatten())
        .unwrap_or_else(|| "eterm".to_string());
    view.set("color_scheme", chosen)?;
    if view.get::<Option<LuaTable>>("palette")?.is_none() {
        view.set("palette", palette.clone())?;
    } else {
        let existing: LuaTable = view.get("palette")?;
        for i in 1..=16 {
            existing.raw_set(i as i64, palette.raw_get::<LuaValue>(i as i64)?)?;
        }
    }

    let foreground = match scheme.get::<Option<String>>("foreground")? {
        Some(color) => hex_to_rgba(lua, color)?,
        None => table_color(lua, style.get("text")?)?,
    };
    let background = match scheme.get::<Option<String>>("background")? {
        Some(color) => hex_to_rgba(lua, color)?,
        None => table_color(lua, style.get("background")?)?,
    };
    let cursor = match scheme.get::<Option<String>>("cursor")? {
        Some(color) => hex_to_rgba(lua, color)?,
        None => table_color(lua, style.get("caret")?)?,
    };
    view.set("default_fg", foreground.clone())?;
    view.set("default_bg", background)?;
    view.set("cursor_color", cursor)?;

    if let Some(buffer) = view.get::<Option<LuaAnyUserData>>("buffer")? {
        buffer.call_method::<bool>("set_palette", (palette_table(lua, view)?, foreground))?;
    }
    Ok(())
}

fn resize_screen(view: &LuaTable, cols: i64, rows: i64) -> LuaResult<()> {
    let cols = cols.max(1);
    let rows = rows.max(1);
    view.set("cols", cols)?;
    view.set("rows", rows)?;
    let buffer: LuaAnyUserData = view.get("buffer")?;
    buffer.call_method::<()>("resize", (cols, rows))?;
    Ok(())
}

fn default_command(lua: &Lua) -> LuaResult<LuaTable> {
    let config = require_table(lua, "core.config")?;
    let plugins: LuaTable = config.get("plugins")?;
    let terminal_cfg: LuaTable = plugins.get("terminal")?;
    let shell = terminal_cfg
        .get::<Option<String>>("shell")?
        .or_else(|| std::env::var("SHELL").ok())
        .unwrap_or_else(|| "sh".to_string());
    let out = lua.create_table()?;
    out.raw_set(1, shell)?;
    if let Some(args) = terminal_cfg.get::<Option<LuaTable>>("shell_args")? {
        let mut idx = 2i64;
        for value in args.sequence_values::<LuaValue>() {
            out.raw_set(idx, value?)?;
            idx += 1;
        }
    }
    Ok(out)
}

fn init(lua: &Lua, (view, options): (LuaTable, LuaTable)) -> LuaResult<()> {
    let style = require_table(lua, "core.style")?;
    let config = require_table(lua, "core.config")?;
    let plugins: LuaTable = config.get("plugins")?;
    let terminal_cfg: LuaTable = plugins.get("terminal")?;
    let common = require_table(lua, "core.common")?;
    let home_encode: LuaFunction = common.get("home_encode")?;
    let cwd = match options.get::<Option<String>>("cwd")? {
        Some(path) => normalize_path(lua, &path)?,
        None => default_cwd(lua)?,
    };
    let title = options
        .get::<Option<String>>("title")?
        .unwrap_or_else(|| format!("Terminal: {}", cwd));
    let title: String = if title.starts_with("Terminal: ") {
        title
    } else {
        format!("Terminal: {}", home_encode.call::<String>(cwd.clone())?)
    };
    let color_scheme = options
        .get::<Option<String>>("color_scheme")?
        .or_else(|| terminal_cfg.get::<Option<String>>("color_scheme").ok().flatten())
        .unwrap_or_else(|| "eterm".to_string());
    let scrollback = terminal_cfg.get::<Option<i64>>("scrollback")?.unwrap_or(5000);
    let placement = options
        .get::<Option<String>>("placement")?
        .or_else(|| terminal_cfg.get::<Option<String>>("open_position").ok().flatten())
        .unwrap_or_else(|| "bottom".to_string());
    let pending_command = match options.get::<Option<LuaTable>>("command")? {
        Some(command) => command,
        None => default_command(lua)?,
    };

    view.set("cursor", "ibeam")?;
    view.set("scrollable", true)?;
    view.set("font", style.get::<LuaValue>("code_font")?)?;
    view.set("cwd", cwd)?;
    view.set("title", title)?;
    view.set("color_scheme", color_scheme.clone())?;
    view.set("cols", 0)?;
    view.set("rows", 0)?;
    view.set("scrollback", scrollback)?;
    view.set("open_placement", placement)?;
    view.set("exit_notified", false)?;
    view.set(
        "spawned_at",
        require_table(lua, "system")?.get::<LuaFunction>("get_time")?.call::<f64>(())?,
    )?;
    let restored = options.get::<Option<bool>>("restored")?.unwrap_or(false);
    view.set("allow_close_on_exit", !restored)?;
    view.set("suppress_startup_exit_notice", restored)?;
    view.set("seen_output", false)?;
    view.set("last_blink", false)?;
    view.set("last_dimensions", "")?;
    view.set("pending_command", pending_command)?;
    apply_color_scheme(lua, &view, Some(color_scheme))?;
    let terminal_buffer = require_table(lua, "terminal_buffer")?;
    let new_fn: LuaFunction = terminal_buffer.get("new")?;
    let palette = palette_table(lua, &view)?;
    let default_fg: LuaTable = view.get("default_fg")?;
    let buffer: LuaAnyUserData = new_fn.call((
        80,
        24,
        scrollback,
        palette,
        default_fg,
    ))?;
    view.set("buffer", buffer)?;
    resize_screen(&view, 80, 24)?;
    Ok(())
}

fn spawn(lua: &Lua, (view, command_argv): (LuaTable, LuaTable)) -> LuaResult<()> {
    let terminal = require_table(lua, "terminal")?;
    let spawn_fn: LuaFunction = terminal.get("spawn")?;
    let opts = lua.create_table()?;
    opts.set("cwd", view.get::<LuaValue>("cwd")?)?;
    opts.set("cols", view.get::<LuaValue>("cols")?)?;
    opts.set("rows", view.get::<LuaValue>("rows")?)?;
    match spawn_fn.call::<LuaAnyUserData>((command_argv, opts)) {
        Ok(handle) => {
            view.set("handle", handle)?;
        }
        Err(err) => {
            let core = require_table(lua, "core")?;
            let error_fn: LuaFunction = core.get("error")?;
            let _ = error_fn.call::<()>(("Failed to start terminal: %s", err.to_string()));
        }
    }
    Ok(())
}

fn scroll_to_bottom(view: &LuaTable, force: bool) -> LuaResult<()> {
    let scrollable_size: f64 = {
        let scrollable: LuaFunction = view.get("get_scrollable_size")?;
        scrollable.call(view.clone())?
    };
    let size: LuaTable = view.get("size")?;
    let target = (scrollable_size - size.get::<f64>("y")?).max(0.0);
    let scroll: LuaTable = view.get("scroll")?;
    let to: LuaTable = scroll.get("to")?;
    to.set("y", target)?;
    if force {
        scroll.set("y", target)?;
    }
    Ok(())
}

fn get_dimensions(lua: &Lua, view: &LuaTable) -> LuaResult<(i64, i64)> {
    let get_char_width: LuaFunction = view.get("get_char_width")?;
    let get_line_height: LuaFunction = view.get("get_line_height")?;
    let style_tbl = require_table(lua, "core.style")?;
    let padding: LuaTable = style_tbl.get("padding")?;
    let size: LuaTable = view.get("size")?;
    let char_w: f64 = get_char_width.call(view.clone())?;
    let line_h: f64 = get_line_height.call(view.clone())?;
    let cols = (((size.get::<f64>("x")? - padding.get::<f64>("x")? * 2.0) / char_w).floor() as i64)
        .max(1);
    let rows = (((size.get::<f64>("y")? - padding.get::<f64>("y")? * 2.0) / line_h).floor() as i64)
        .max(1);
    Ok((cols, rows))
}

fn node_has_terminal(node: &LuaTable) -> LuaResult<bool> {
    let views: LuaTable = node.get("views")?;
    for view in views.sequence_values::<LuaTable>() {
        let view = view?;
        let tostring_fn = match view.get::<Option<LuaFunction>>("__tostring")? {
            Some(func) => func,
            None => continue,
        };
        let name: String = tostring_fn.call(view.clone())?;
        if name == "TerminalView" {
            return Ok(true);
        }
    }
    Ok(false)
}

fn normalize_project_path(lua: &Lua, path: String) -> LuaResult<Option<String>> {
    let core = require_table(lua, "core")?;
    let project_for_path = match core.get::<Option<LuaFunction>>("project_for_path")? {
        Some(func) => func,
        None => return Ok(None),
    };
    let project: Option<LuaTable> = project_for_path.call(path)?;
    Ok(project.and_then(|project| project.get::<Option<String>>("path").ok().flatten()))
}

fn terminal_matches(
    lua: &Lua,
    view: &LuaTable,
    placement: &str,
    cwd: &str,
    reuse_mode: &str,
) -> LuaResult<bool> {
    let tostring_fn = match view.get::<Option<LuaFunction>>("__tostring")? {
        Some(func) => func,
        None => return Ok(false),
    };
    let name: String = tostring_fn.call(view.clone())?;
    if name != "TerminalView" {
        return Ok(false);
    }
    if reuse_mode == "pane" {
        return Ok(view.get::<String>("open_placement")? == placement);
    }
    if reuse_mode == "project" {
        let lhs = view.get::<Option<String>>("cwd")?.unwrap_or_default();
        return Ok(normalize_project_path(lua, lhs)? == normalize_project_path(lua, cwd.to_string())?);
    }
    Ok(true)
}

fn walk_nodes<F>(node: &LuaTable, f: &mut F) -> LuaResult<()>
where
    F: FnMut(&LuaTable) -> LuaResult<bool>,
{
    if f(node)? {
        return Ok(());
    }
    let node_type: String = node.get("type")?;
    if node_type == "leaf" {
        return Ok(());
    }
    let a: LuaTable = node.get("a")?;
    walk_nodes(&a, f)?;
    let b: LuaTable = node.get("b")?;
    walk_nodes(&b, f)?;
    Ok(())
}

fn find_reuse_target(
    lua: &Lua,
    placement: &str,
    cwd: &str,
    reuse_mode: &str,
) -> LuaResult<(Option<LuaTable>, Option<LuaTable>)> {
    let core = require_table(lua, "core")?;
    let root_view: LuaTable = core.get("root_view")?;
    let root_node: LuaTable = root_view.get("root_node")?;
    let mut match_view = None;
    let mut match_node = None;
    walk_nodes(&root_node, &mut |node| {
        if match_view.is_some() || !node_has_terminal(node)? {
            return Ok(false);
        }
        let views: LuaTable = node.get("views")?;
        for view in views.sequence_values::<LuaTable>() {
            let view = view?;
            if terminal_matches(lua, &view, placement, cwd, reuse_mode)? {
                match_view = Some(view);
                match_node = Some(node.clone());
                return Ok(true);
            }
        }
        Ok(false)
    })?;
    Ok((match_view, match_node))
}

fn has_no_locked_children(node: &LuaTable) -> LuaResult<bool> {
    if node.get::<Option<LuaValue>>("locked")?.is_some() {
        return Ok(false);
    }
    let node_type: String = node.get("type")?;
    if node_type == "leaf" {
        return Ok(true);
    }
    let a: LuaTable = node.get("a")?;
    let b: LuaTable = node.get("b")?;
    Ok(has_no_locked_children(&a)? && has_no_locked_children(&b)?)
}

fn get_unlocked_root(node: &LuaTable) -> LuaResult<Option<LuaTable>> {
    let node_type: String = node.get("type")?;
    if node_type == "leaf" {
        return if node.get::<Option<LuaValue>>("locked")?.is_none() {
            Ok(Some(node.clone()))
        } else {
            Ok(None)
        };
    }
    if has_no_locked_children(node)? {
        return Ok(Some(node.clone()));
    }
    let a: LuaTable = node.get("a")?;
    if let Some(root) = get_unlocked_root(&a)? {
        return Ok(Some(root));
    }
    let b: LuaTable = node.get("b")?;
    get_unlocked_root(&b)
}

fn add_view_in_workspace(lua: &Lua, view: LuaTable, placement: &str) -> LuaResult<LuaTable> {
    let core = require_table(lua, "core")?;
    let root_view: LuaTable = core.get("root_view")?;
    let root_node: LuaTable = root_view.get("root_node")?;
    let workspace_root = match get_unlocked_root(&root_node)? {
        Some(node) => node,
        None => {
            root_view.call_method::<LuaTable>("add_view", (view.clone(), placement.to_string()))?;
            return Ok(view);
        }
    };

    let split_type = match placement {
        "left" | "right" => "hsplit",
        "top" | "bottom" => "vsplit",
        _ => "",
    };
    let edge_key = if placement == "left" || placement == "top" {
        "a"
    } else {
        "b"
    };
    if !split_type.is_empty() {
        let root_type: String = workspace_root.get("type")?;
        if root_type == split_type {
            if let Some(edge) = workspace_root.get::<Option<LuaTable>>(edge_key)? {
                let edge_type: String = edge.get("type")?;
                if edge_type == "leaf" && edge.get::<Option<LuaValue>>("locked")?.is_none() {
                    edge.call_method::<()>("add_view", view.clone())?;
                    root_node.call_method::<()>("update_layout", ())?;
                    let set_active_view: LuaFunction = core.get("set_active_view")?;
                    set_active_view.call::<()>(view.clone())?;
                    return Ok(view);
                }
            }
        }
    }

    if split_type.is_empty() {
        root_view.call_method::<LuaTable>("add_view", (view.clone(), placement.to_string()))?;
        return Ok(view);
    }

    let node_ctor = require_table(lua, "core.node")?;
    let existing = match call_callable(lua, LuaValue::Table(node_ctor.clone()), LuaMultiValue::new())? {
        LuaValue::Table(t) => t,
        _ => return Err(LuaError::RuntimeError("failed to create terminal split node".into())),
    };
    existing.call_method::<()>("consume", workspace_root.clone())?;
    let sibling = match call_callable(lua, LuaValue::Table(node_ctor.clone()), LuaMultiValue::new())? {
        LuaValue::Table(t) => t,
        _ => return Err(LuaError::RuntimeError("failed to create terminal sibling node".into())),
    };
    let views = lua.create_table()?;
    sibling.set("views", views)?;
    sibling.call_method::<()>("add_view", view.clone())?;
    let new_root = match call_callable(
        lua,
        LuaValue::Table(node_ctor),
        LuaMultiValue::from_vec(vec![LuaValue::String(lua.create_string(split_type)?)])
    )? {
        LuaValue::Table(t) => t,
        _ => return Err(LuaError::RuntimeError("failed to create terminal root node".into())),
    };
    new_root.set("a", existing.clone())?;
    new_root.set("b", sibling.clone())?;
    if placement == "left" || placement == "top" {
        new_root.set("a", sibling.clone())?;
        new_root.set("b", existing.clone())?;
    }
    workspace_root.call_method::<()>("consume", new_root)?;
    root_node.call_method::<()>("update_layout", ())?;
    let set_active_view: LuaFunction = core.get("set_active_view")?;
    set_active_view.call::<()>(view.clone())?;
    Ok(view)
}

fn open(
    lua: &Lua,
    (class, cwd, command_argv, title, placement): (
        LuaTable,
        Option<String>,
        Option<LuaTable>,
        Option<String>,
        Option<String>,
    ),
) -> LuaResult<LuaTable> {
    let config = require_table(lua, "core.config")?;
    let plugins: LuaTable = config.get("plugins")?;
    let terminal_cfg: LuaTable = plugins.get("terminal")?;
    let cwd = match cwd {
        Some(path) => normalize_path(lua, &path)?,
        None => default_cwd(lua)?,
    };
    let placement = placement
        .or_else(|| terminal_cfg.get::<Option<String>>("open_position").ok().flatten())
        .unwrap_or_else(|| "bottom".to_string());
    let reuse_mode = terminal_cfg
        .get::<Option<String>>("reuse_mode")?
        .unwrap_or_else(|| "pane".to_string());
    if reuse_mode != "never" {
        let (reuse_view, reuse_node) = find_reuse_target(lua, &placement, &cwd, &reuse_mode)?;
        if let Some(reuse_view) = reuse_view {
            if reuse_mode == "view" {
                let core = require_table(lua, "core")?;
                let set_active_view: LuaFunction = core.get("set_active_view")?;
                set_active_view.call::<()>(reuse_view.clone())?;
                return Ok(reuse_view);
            }
        }
        if let Some(reuse_node) = reuse_node {
            let opts = lua.create_table()?;
            opts.set("cwd", cwd.clone())?;
            opts.set("title", title.clone())?;
            opts.set("placement", placement.clone())?;
            if let Some(command_argv) = command_argv.clone() {
                opts.set("command", command_argv)?;
            }
            let view = match call_callable(
                lua,
                LuaValue::Table(class.clone()),
                LuaMultiValue::from_vec(vec![LuaValue::Table(opts)]),
            )? {
                LuaValue::Table(t) => t,
                _ => return Err(LuaError::RuntimeError("failed to instantiate TerminalView".into())),
            };
            reuse_node.call_method::<()>("add_view", view.clone())?;
            let core = require_table(lua, "core")?;
            let root_view: LuaTable = core.get("root_view")?;
            let root_node: LuaTable = root_view.get("root_node")?;
            root_node.call_method::<()>("update_layout", ())?;
            let set_active_view: LuaFunction = core.get("set_active_view")?;
            set_active_view.call::<()>(view.clone())?;
            return Ok(view);
        }
    }
    let opts = lua.create_table()?;
    opts.set("cwd", cwd.clone())?;
    opts.set("title", title)?;
    opts.set("placement", placement.clone())?;
    if let Some(command_argv) = command_argv {
        opts.set("command", command_argv)?;
    }
    let view = match call_callable(
        lua,
        LuaValue::Table(class),
        LuaMultiValue::from_vec(vec![LuaValue::Table(opts)]),
    )? {
        LuaValue::Table(t) => t,
        _ => return Err(LuaError::RuntimeError("failed to instantiate TerminalView".into())),
    };
    add_view_in_workspace(lua, view, &placement)
}

fn update(lua: &Lua, view: LuaTable) -> LuaResult<()> {
    let (cols, rows) = get_dimensions(lua, &view)?;
    let dim_key = format!("{cols}x{rows}");
    let size: LuaTable = view.get("size")?;
    if view.get::<Option<LuaAnyUserData>>("handle")?.is_none()
        && view.get::<Option<LuaTable>>("pending_command")?.is_some()
        && size.get::<f64>("x")? > 0.0
        && size.get::<f64>("y")? > 0.0
    {
        view.set("last_dimensions", dim_key.clone())?;
        resize_screen(&view, cols, rows)?;
        let command = view.get::<LuaTable>("pending_command")?;
        spawn(lua, (view.clone(), command))?;
        view.set("pending_command", LuaValue::Nil)?;
    }
    if view.get::<String>("last_dimensions")? != dim_key {
        view.set("last_dimensions", dim_key)?;
        resize_screen(&view, cols, rows)?;
        if let Some(handle) = view.get::<Option<LuaAnyUserData>>("handle")? {
            let _ = handle.call_method::<bool>("resize", (cols as u16, rows as u16));
        }
    }

    let get_scrollable_size: LuaFunction = view.get("get_scrollable_size")?;
    let scrollable_size: f64 = get_scrollable_size.call(view.clone())?;
    let get_line_height: LuaFunction = view.get("get_line_height")?;
    let line_height: f64 = get_line_height.call(view.clone())?;
    let scroll: LuaTable = view.get("scroll")?;
    let to: LuaTable = scroll.get("to")?;
    let at_bottom = to.get::<f64>("y")?
        >= (scrollable_size - size.get::<f64>("y")? - line_height).max(0.0);

    if let Some(handle) = view.get::<Option<LuaAnyUserData>>("handle")? {
        for _ in 0..256 {
            let chunk = handle.call_method::<LuaValue>("read", 4096)?;
            let chunk = match chunk {
                LuaValue::Nil => break,
                LuaValue::String(s) if s.as_bytes().is_empty() => break,
                LuaValue::String(s) => s.to_str()?.to_string(),
                _ => break,
            };
            view.set("allow_close_on_exit", true)?;
            view.set("seen_output", true)?;
            view.set("suppress_startup_exit_notice", false)?;
            let buffer: LuaAnyUserData = view.get("buffer")?;
            let replies: LuaValue = buffer.call_method("process_output_replies", chunk)?;
            if let LuaValue::String(replies) = replies {
                if !replies.as_bytes().is_empty() && handle.call_method::<bool>("running", ())? {
                    let _ = handle.call_method::<LuaValue>("write", replies);
                }
            }
            let core = require_table(lua, "core")?;
            core.set("redraw", true)?;
        }

        if !view.get::<bool>("allow_close_on_exit")? && handle.call_method::<bool>("running", ())? {
            let spawned_at = view.get::<f64>("spawned_at")?;
            let now: f64 = require_table(lua, "system")?.get::<LuaFunction>("get_time")?.call(())?;
            if now - spawned_at > 1.0 {
                view.set("allow_close_on_exit", true)?;
            }
        }

        if !handle.call_method::<bool>("running", ())? && !view.get::<bool>("exit_notified")? {
            view.set("exit_notified", true)?;
            let config = require_table(lua, "core.config")?;
            let plugins: LuaTable = config.get("plugins")?;
            let terminal_cfg: LuaTable = plugins.get("terminal")?;
            let close_on_exit = terminal_cfg.get::<Option<bool>>("close_on_exit")?.unwrap_or(true);
            if close_on_exit && view.get::<bool>("allow_close_on_exit")? {
                let core = require_table(lua, "core")?;
                let root_view: LuaTable = core.get("root_view")?;
                let root_node: LuaTable = root_view.get("root_node")?;
                if let Some(node) = root_node.call_method::<Option<LuaTable>>("get_node_for_view", view.clone())? {
                    node.call_method::<()>("close_view", (root_node, view.clone()))?;
                    return Ok(());
                }
            }
            if !view.get::<bool>("suppress_startup_exit_notice")? {
                let core = require_table(lua, "core")?;
                let status_view: LuaTable = core.get("status_view")?;
                let style = require_table(lua, "core.style")?;
                let text_color: LuaValue = style.get("text")?;
                let code = handle.call_method::<LuaValue>("returncode", ())?;
                let code_text = match code {
                    LuaValue::Integer(i) => i.to_string(),
                    LuaValue::Number(n) => n.to_string(),
                    _ => "?".to_string(),
                };
                status_view.call_method::<()>(
                    "show_message",
                    ("i", text_color, format!("Terminal exited with code {code_text}")),
                )?;
            }
            let core = require_table(lua, "core")?;
            core.set("redraw", true)?;
        }
    }

    if at_bottom {
        scroll_to_bottom(&view, false)?;
    }
    Ok(())
}

fn draw_cursor(lua: &Lua, view: LuaTable) -> LuaResult<()> {
    let core = require_table(lua, "core")?;
    if core.get::<LuaValue>("active_view")? != LuaValue::Table(view.clone()) {
        return Ok(());
    }
    let system = require_table(lua, "system")?;
    let window_has_focus: LuaFunction = system.get("window_has_focus")?;
    if !window_has_focus.call::<bool>(core.get::<LuaValue>("window")?)? {
        return Ok(());
    }
    let config = require_table(lua, "core.config")?;
    let disable_blink = config.get::<Option<bool>>("disable_blink")?.unwrap_or(false);
    let blink_period = config.get::<Option<f64>>("blink_period")?.unwrap_or(0.8);
    let get_time: LuaFunction = system.get("get_time")?;
    let now: f64 = get_time.call(())?;
    let blink_start = core.get::<Option<f64>>("blink_start")?.unwrap_or(now);
    if !disable_blink && ((now - blink_start) % blink_period) >= blink_period / 2.0 {
        return Ok(());
    }
    let buffer: LuaAnyUserData = view.get("buffer")?;
    let cursor: LuaTable = buffer.call_method("cursor", ())?;
    if cursor.get::<Option<bool>>("visible")? == Some(false) {
        return Ok(());
    }
    let get_line_height: LuaFunction = view.get("get_line_height")?;
    let line_height: f64 = get_line_height.call(view.clone())?;
    let get_char_width: LuaFunction = view.get("get_char_width")?;
    let char_width: f64 = get_char_width.call(view.clone())?;
    let style = require_table(lua, "core.style")?;
    let padding: LuaTable = style.get("padding")?;
    let position: LuaTable = view.get("position")?;
    let size: LuaTable = view.get("size")?;
    let scroll: LuaTable = view.get("scroll")?;
    let row_index = cursor.get::<i64>("history")? + cursor.get::<i64>("row")?;
    let y = position.get::<f64>("y")?
        + padding.get::<f64>("y")?
        + (row_index as f64 - 1.0) * line_height
        - scroll.get::<f64>("y")?;
    if y + line_height < position.get::<f64>("y")? || y > position.get::<f64>("y")? + size.get::<f64>("y")? {
        return Ok(());
    }
    let x = position.get::<f64>("x")?
        + padding.get::<f64>("x")?
        + (cursor.get::<i64>("col")? as f64 - 1.0) * char_width;
    let renderer = require_table(lua, "renderer")?;
    let draw_rect: LuaFunction = renderer.get("draw_rect")?;
    let caret_width = style.get::<Option<f64>>("caret_width")?.unwrap_or(1.0).max(1.0);
    draw_rect.call::<()>((
        x,
        y,
        caret_width,
        line_height,
        view.get::<LuaValue>("cursor_color")?,
    ))?;
    Ok(())
}

fn switch_color_scheme(lua: &Lua, (view, direction): (LuaTable, i64)) -> LuaResult<()> {
    let schemes = require_table(lua, "plugins.terminal.colors")?;
    let mut names = Vec::new();
    for pair in schemes.pairs::<String, LuaValue>() {
        let (name, _) = pair?;
        names.push(name);
    }
    names.sort();
    if names.is_empty() {
        return Ok(());
    }
    let current_name = view.get::<Option<String>>("color_scheme")?.unwrap_or_else(|| names[0].clone());
    let current_idx = names.iter().position(|name| name == &current_name).unwrap_or(0) as i64;
    let next_idx = ((current_idx + direction).rem_euclid(names.len() as i64)) as usize;
    let next_name = names[next_idx].clone();
    apply_color_scheme(lua, &view, Some(next_name.clone()))?;
    let config = require_table(lua, "core.config")?;
    let plugins: LuaTable = config.get("plugins")?;
    let terminal_cfg: LuaTable = plugins.get("terminal")?;
    terminal_cfg.set("color_scheme", next_name.clone())?;
    let core = require_table(lua, "core")?;
    let status_view: LuaTable = core.get("status_view")?;
    let style = require_table(lua, "core.style")?;
    status_view.call_method::<()>(
        "show_message",
        (
            "i",
            style.get::<LuaValue>("text")?,
            format!("Terminal color scheme: {next_name}"),
        ),
    )?;
    core.set("redraw", true)?;
    Ok(())
}

fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    let module = lua.create_table()?;
    module.set("init", lua.create_function(init)?)?;
    module.set("apply_color_scheme", lua.create_function(|lua, (view, name): (LuaTable, String)| {
        apply_color_scheme(lua, &view, Some(name))
    })?)?;
    module.set("resize_screen", lua.create_function(|_, (view, cols, rows): (LuaTable, i64, i64)| {
        resize_screen(&view, cols, rows)
    })?)?;
    module.set("spawn", lua.create_function(spawn)?)?;
    module.set("scroll_to_bottom", lua.create_function(|_, (view, force): (LuaTable, Option<bool>)| {
        scroll_to_bottom(&view, force.unwrap_or(false))
    })?)?;
    module.set("get_dimensions", lua.create_function(|lua, view: LuaTable| get_dimensions(lua, &view))?)?;
    module.set("update", lua.create_function(update)?)?;
    module.set("draw_cursor", lua.create_function(draw_cursor)?)?;
    module.set("open", lua.create_function(open)?)?;
    module.set("switch_color_scheme", lua.create_function(switch_color_scheme)?)?;
    Ok(module)
}

pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let package: LuaTable = lua.globals().get("package")?;
    let preload: LuaTable = package.get("preload")?;
    let native_key = lua.create_registry_value(make_module(lua)?)?;
    preload.set(
        "terminal_view_native",
        lua.create_function(move |lua, ()| lua.registry_value::<LuaTable>(&native_key))?,
    )?;
    preload.set(
        "plugins.terminal.view",
        lua.create_function(|lua, ()| lua.load(BOOTSTRAP).set_name("plugins.terminal.view").eval::<LuaValue>())?,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stub_require(lua: &Lua) -> LuaResult<()> {
        let package = lua.create_table()?;
        let loaded = lua.create_table()?;
        let preload = lua.create_table()?;
        package.set("loaded", loaded.clone())?;
        package.set("preload", preload)?;
        lua.globals().set("package", package)?;

        let color_schemes = lua.create_table()?;
        let eterm = lua.create_table()?;
        let palette = lua.create_table()?;
        for (idx, color) in [
            "#000000", "#111111", "#222222", "#333333",
            "#444444", "#555555", "#666666", "#777777",
            "#888888", "#999999", "#aaaaaa", "#bbbbbb",
            "#cccccc", "#dddddd", "#eeeeee", "#ffffff",
        ]
        .into_iter()
        .enumerate()
        {
            palette.raw_set((idx + 1) as i64, color)?;
        }
        eterm.set("palette", palette)?;
        eterm.set("foreground", "#c0c0c0")?;
        eterm.set("background", "#101010")?;
        eterm.set("cursor", "#f0f0f0")?;
        color_schemes.set("eterm", eterm)?;
        loaded.set("plugins.terminal.colors", color_schemes)?;

        let common = lua.create_table()?;
        common.set(
            "normalize_path",
            lua.create_function(|_, path: String| Ok(path))?,
        )?;
        common.set(
            "dirname",
            lua.create_function(|_, path: String| {
                Ok(path.rsplit_once('/').map(|(dir, _)| dir.to_string()).unwrap_or(path))
            })?,
        )?;
        common.set(
            "home_encode",
            lua.create_function(|_, path: String| Ok(path))?,
        )?;
        common.set(
            "color",
            lua.create_function(|_, hex: String| {
                let hex = hex.trim_start_matches('#');
                let bytes = |s: &str| i64::from_str_radix(s, 16).unwrap();
                let (r, g, b, a) = match hex.len() {
                    6 => (bytes(&hex[0..2]), bytes(&hex[2..4]), bytes(&hex[4..6]), 255),
                    8 => (bytes(&hex[0..2]), bytes(&hex[2..4]), bytes(&hex[4..6]), bytes(&hex[6..8])),
                    _ => (0, 0, 0, 255),
                };
                Ok((r, g, b, a))
            })?,
        )?;
        loaded.set("core.common", common)?;

        let padding = lua.create_table()?;
        padding.set("x", 4)?;
        padding.set("y", 4)?;
        let style = lua.create_table()?;
        style.set("code_font", lua.create_table()?)?;
        let color = |r, g, b, a| -> LuaResult<LuaTable> {
            let t = lua.create_table()?;
            t.raw_set(1, r)?;
            t.raw_set(2, g)?;
            t.raw_set(3, b)?;
            t.raw_set(4, a)?;
            Ok(t)
        };
        style.set("text", color(200, 200, 200, 255)?)?;
        style.set("background", color(16, 16, 16, 255)?)?;
        style.set("caret", color(255, 255, 255, 255)?)?;
        style.set("padding", padding)?;
        loaded.set("core.style", style)?;

        let terminal_cfg = lua.create_table()?;
        terminal_cfg.set("color_scheme", "eterm")?;
        terminal_cfg.set("scrollback", 5000)?;
        terminal_cfg.set("open_position", "bottom")?;
        terminal_cfg.set("shell", "sh")?;
        terminal_cfg.set("shell_args", lua.create_table()?)?;
        let plugins = lua.create_table()?;
        plugins.set("terminal", terminal_cfg)?;
        let config = lua.create_table()?;
        config.set("plugins", plugins)?;
        loaded.set("core.config", config)?;

        let system = lua.create_table()?;
        system.set("get_time", lua.create_function(|_, ()| Ok(0.0))?)?;
        loaded.set("system", system)?;

        let terminal_buffer = crate::api::terminal_buffer::make_module(lua)?;
        loaded.set("terminal_buffer", terminal_buffer)?;

        let require = lua.create_function(|lua, name: String| {
            let package: LuaTable = lua.globals().get("package")?;
            let loaded: LuaTable = package.get("loaded")?;
            loaded.get::<LuaValue>(name)
        })?;
        lua.globals().set("require", require)?;
        Ok(())
    }

    #[test]
    fn init_builds_full_terminal_palette_and_buffer() {
        let lua = Lua::new();
        stub_require(&lua).expect("stub_require");
        let (_, built_palette) = make_palette(&lua, Some("eterm".to_string())).expect("make_palette");
        assert_eq!(built_palette.raw_len(), 16);
        for i in 1..=16 {
            let value: LuaValue = built_palette.raw_get(i).expect("palette slot");
            let color = match value {
                LuaValue::Table(t) => t,
                other => panic!("palette slot {i} was not a table: {other:?}"),
            };
            let _: i64 = color.raw_get(1).expect("color r");
            let _: i64 = color.raw_get(2).expect("color g");
            let _: i64 = color.raw_get(3).expect("color b");
            let _: i64 = color.raw_get(4).expect("color a");
        }
        let view = lua.create_table().expect("view");
        let style = require_table(&lua, "core.style").expect("style");
        view.set("font", style.get::<LuaValue>("code_font").expect("code_font"))
            .expect("set font");
        view.set("cwd", "/tmp").expect("set cwd");
        view.set("title", "Terminal: /tmp").expect("set title");
        view.set("scrollback", 5000).expect("set scrollback");
        view.set("color_scheme", "eterm").expect("set color scheme");
        apply_color_scheme(&lua, &view, Some("eterm".to_string())).expect("apply_color_scheme");
        let default_fg: LuaTable = view.get("default_fg").expect("default_fg");
        let copied_palette = palette_table(&lua, &view).expect("palette_table");
        let terminal_buffer = require_table(&lua, "terminal_buffer").expect("terminal_buffer");
        let new_fn: LuaFunction = terminal_buffer.get("new").expect("new");
        let buffer: LuaAnyUserData = new_fn
            .call((80, 24, 5000, copied_palette, default_fg))
            .expect("terminal_buffer.new");
        view.set("buffer", buffer).expect("set buffer");
        resize_screen(&view, 80, 24).expect("resize_screen");
        let buffer: LuaAnyUserData = view.get("buffer").expect("buffer");
        let total_rows: i64 = buffer.call_method("total_rows", ()).expect("total_rows");
        assert_eq!(total_rows, 24);
        let palette: LuaTable = view.get("palette").expect("palette");
        for i in 1..=16 {
            let color: LuaTable = palette.raw_get(i).expect("stored color");
            let _: i64 = color.raw_get(1).expect("stored r");
            let _: i64 = color.raw_get(2).expect("stored g");
            let _: i64 = color.raw_get(3).expect("stored b");
            let _: i64 = color.raw_get(4).expect("stored a");
        }
    }
}
