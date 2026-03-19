use mlua::prelude::*;

/// Thin Lua bootstrap: View:extend OOP + StatusViewItem class +
/// update_active_items (kept in Lua for predicate/font callbacks) +
/// draw methods (kept in Lua for renderer.* calls) +
/// register_docview_items / register_command_items.
/// All pure-logic functions delegate to the Rust native module.
const BOOTSTRAP: &str = r#"
local core = require "core"
local command = require "core.command"
local common = require "core.common"
local config = require "core.config"
local style = require "core.style"
local View = require "core.view"
local Object = require "core.object"
local native = require "statusview_native"

---@alias core.statusview.styledtext table<integer, renderer.font|renderer.color|string>
---@alias core.statusview.position '"left"' | '"right"'

---@class core.statusview : core.view
local StatusView = View:extend()

function StatusView:__tostring() return "StatusView" end

StatusView.separator  = "      "
StatusView.separator2 = "   |   "

---@class core.statusview.item : core.object
local StatusViewItem = Object:extend()

function StatusViewItem:__tostring() return "StatusViewItem" end

StatusViewItem.LEFT = 1
StatusViewItem.RIGHT = 2

function StatusViewItem:new(options)
  self:set_predicate(options.predicate)
  self.name = options.name
  self.alignment = options.alignment or StatusView.Item.LEFT
  self.command = type(options.command) == "string" and options.command or nil
  self.tooltip = options.tooltip or ""
  self.on_click = type(options.command) == "function" and options.command or nil
  self.on_draw = nil
  self.background_color = nil
  self.background_color_hover = nil
  self.visible = options.visible == nil and true or options.visible
  self.active = false
  self.x = 0
  self.w = 0
  self.separator = options.separator or StatusView.separator
  self.get_item = options.get_item
end

function StatusViewItem:get_item() return {} end
function StatusViewItem:hide() self.visible = false end
function StatusViewItem:show() self.visible = true end

function StatusViewItem:set_predicate(predicate)
  self.predicate = command.generate_predicate(predicate)
end

StatusView.Item = StatusViewItem

function StatusView:new()
  StatusView.super.new(self)
  native.init(self)
  self:register_docview_items()
  self:register_command_items()
end

-- Item management: create item in Lua (needs OOP constructor), delegate
-- position math to Rust.
function StatusView:add_item(options)
  assert(self:get_item(options.name) == nil, "status item already exists: " .. options.name)
  local item = StatusView.Item(options)
  local pos = native.normalize_position(
    self.items,
    options.position or -1,
    options.alignment or StatusView.Item.LEFT
  )
  table.insert(self.items, pos, item)
  return item
end

function StatusView:get_item(name)      return native.get_item(self, name) end
function StatusView:remove_item(name)   return native.remove_item(self, name) end

function StatusView:move_item(name, position, alignment)
  return native.move_item(self, name, position, alignment)
end

function StatusView:order_items(names)       native.order_items(self, names) end
function StatusView:get_items_list(alignment) return native.get_items_list(self, alignment) end

function StatusView:hide()   self.visible = false end
function StatusView:show()   self.visible = true end
function StatusView:toggle() self.visible = not self.visible end

function StatusView:hide_items(names)
  if type(names) == "string" then names = {names} end
  if not names then
    for _, item in ipairs(self.items) do item:hide() end
    return
  end
  for _, name in ipairs(names) do
    local item = self:get_item(name)
    if item then item:hide() end
  end
end

function StatusView:show_items(names)
  if type(names) == "string" then names = {names} end
  if not names then
    for _, item in ipairs(self.items) do item:show() end
    return
  end
  for _, name in ipairs(names) do
    local item = self:get_item(name)
    if item then item:show() end
  end
end

function StatusView:show_message(icon, icon_color, text)
  native.show_message(self, icon, icon_color, text)
end

function StatusView:display_messages(enable)
  self.hide_messages = not enable
end

function StatusView:show_tooltip(text)
  self.tooltip = type(text) == "table" and text or { text }
  self.tooltip_mode = true
end

function StatusView:remove_tooltip()
  self.tooltip_mode = false
end

function StatusView:drag_panel(panel, dx)         native.drag_panel(self, panel, dx) end
function StatusView:get_hovered_panel(x, y)       return native.get_hovered_panel(self, x, y) end
function StatusView:get_item_visible_area(item)   return native.get_item_visible_area(self, item) end

-- ── Layout helpers used by update_active_items ───────────────────────────────

local function text_width(font, _, text, _, x)
  return x + font:get_width(text)
end

local function draw_items_with(self, items, x, y, draw_fn)
  local font = style.font
  local color = style.text
  for _, item in ipairs(items) do
    if Object.is(item, renderer.font) then
      font = item
    elseif type(item) == "table" then
      color = item
    else
      x = draw_fn(font, color, item, nil, x, y, 0, self.size.y)
    end
  end
  return x
end

local function measure_styled(self, items)
  return draw_items_with(self, items, 0, 0, text_width)
end

local function styled_text_equals(a, b)
  if a == b then return true end
  if type(a) ~= "table" or type(b) ~= "table" then return false end
  if #a ~= #b then return false end
  for i = 1, #a do
    if a[i] ~= b[i] then return false end
  end
  return true
end

local function remove_spacing(self, styled_text)
  if not Object.is(styled_text[1], renderer.font)
    and type(styled_text[1]) == "table"
    and (styled_text[2] == self.separator or styled_text[2] == self.separator2)
  then
    table.remove(styled_text, 1)
    table.remove(styled_text, 1)
  end
  if not Object.is(styled_text[#styled_text-1], renderer.font)
    and type(styled_text[#styled_text-1]) == "table"
    and (styled_text[#styled_text] == self.separator or styled_text[#styled_text] == self.separator2)
  then
    table.remove(styled_text, #styled_text)
    table.remove(styled_text, #styled_text)
  end
end

local function add_spacing(self, destination, separator, alignment, x)
  local space = StatusView.Item({name = "space", alignment = alignment})
  space.cached_item = separator == self.separator
    and { style.text, separator }
    or  { style.dim,  separator }
  space.x = x
  if separator == self.separator then
    self._separator_width = self._separator_width or measure_styled(self, space.cached_item)
    space.w = self._separator_width
  else
    self._separator2_width = self._separator2_width or measure_styled(self, space.cached_item)
    space.w = self._separator2_width
  end
  table.insert(destination, space)
  return space
end

-- update_active_items stays in Lua: it calls item.predicate() and
-- font:get_width() on every frame, both of which are Lua method calls.
function StatusView:update_active_items()
  local x = self:get_content_offset()
  local rx = x + self.size.x
  local lx = x
  local rw, lw = 0, 0

  self.active_items = {}
  local lfirst, rfirst = true, true

  for _, item in ipairs(self.items) do
    local previous_cached_item = item.cached_item
    if item.visible and item:predicate() then
      local styled_text = type(item.get_item) == "function"
        and item.get_item(item) or item.get_item

      if #styled_text > 0 then remove_spacing(self, styled_text) end

      if #styled_text > 0 or item.on_draw then
        item.active = true
        local hovered = self.hovered_item == item
        if item.alignment == StatusView.Item.LEFT then
          if not lfirst then
            local space = add_spacing(self, self.active_items, item.separator, item.alignment, lx)
            lw = lw + space.w; lx = lx + space.w
          else lfirst = false end
          if item.on_draw then
            item.w = item.on_draw(lx, self.position.y, self.size.y, hovered, true)
          elseif styled_text_equals(previous_cached_item, styled_text) and item.cached_width then
            item.w = item.cached_width
          else
            item.w = measure_styled(self, styled_text)
          end
          item.x = lx; lw = lw + item.w; lx = lx + item.w
        else
          if not rfirst then
            local space = add_spacing(self, self.active_items, item.separator, item.alignment, rx)
            rw = rw + space.w; rx = rx + space.w
          else rfirst = false end
          if item.on_draw then
            item.w = item.on_draw(rx, self.position.y, self.size.y, hovered, true)
          elseif styled_text_equals(previous_cached_item, styled_text) and item.cached_width then
            item.w = item.cached_width
          else
            item.w = measure_styled(self, styled_text)
          end
          item.x = rx; rw = rw + item.w; rx = rx + item.w
        end
        item.cached_item = styled_text
        item.cached_width = item.w
        table.insert(self.active_items, item)
      else
        item.active = false
        item.cached_item = {}
        item.cached_width = 0
      end
    else
      item.active = false
      item.cached_item = {}
      item.cached_width = 0
    end
  end

  self.r_left_width, self.r_right_width = lw, rw
  native.apply_panel_layout(self, lw, rw)

  for _, item in ipairs(self.active_items) do
    if item.alignment == StatusView.Item.RIGHT then
      item.x = item.x - self.right_width - (style.padding.x * 2)
    end
    item.visible_x, item.visible_w = self:get_item_visible_area(item)
  end
end

function StatusView:on_mouse_pressed(button, x, y, clicks)
  return native.on_mouse_pressed(self, button, x, y, clicks)
end

function StatusView:on_mouse_left()
  StatusView.super.on_mouse_left(self)
  self.hovered_item = {}
end

function StatusView:on_mouse_moved(x, y, dx, dy)
  native.on_mouse_moved(self, x, y, dx, dy)
end

function StatusView:on_mouse_released(button, x, y)
  native.on_mouse_released(self, button, x, y)
end

function StatusView:on_mouse_wheel(y, x)
  if not self.visible or self.hovered_panel == "" then return end
  if x ~= 0 then
    self:drag_panel(self.hovered_panel, x * self.left_width / 10)
  else
    self:drag_panel(self.hovered_panel, y * self.left_width / 10)
  end
end

function StatusView:update()
  if not self.visible and self.size.y <= 0 then return end
  if not self.visible and self.size.y > 0 then
    self:move_towards(self.size, "y", 0, nil, "statusbar")
    return
  end

  local height = style.font:get_height() + style.padding.y * 2
  if self.size.y + 1 < height then
    self:move_towards(self.size, "y", height, nil, "statusbar")
  else
    self.size.y = height
  end

  if system.get_time() < self.message_timeout then
    self.scroll.to.y = self.size.y
  else
    self.scroll.to.y = 0
  end

  StatusView.super.update(self)
  self:update_active_items()
end

-- ── Drawing (stays in Lua; calls renderer.* and style.*) ─────────────────────

function StatusView:draw_items(items, right_align, xoffset, yoffset)
  local x, y = self:get_content_offset()
  x = x + (xoffset or 0)
  y = y + (yoffset or 0)
  if right_align then
    local w = draw_items_with(self, items, 0, 0, text_width)
    x = x + self.size.x - w - style.padding.x
    draw_items_with(self, items, x, y, common.draw_text)
  else
    x = x + style.padding.x
    draw_items_with(self, items, x, y, common.draw_text)
  end
end

function StatusView:draw_item_tooltip(item)
  core.root_view:defer_draw(function()
    local text = item.tooltip
    local w = style.font:get_width(text)
    local h = style.font:get_height()
    local x = self.pointer.x - (w / 2) - (style.padding.x * 2)
    if x < 0 then x = 0 end
    if (x + w + (style.padding.x * 3)) > self.size.x then
      x = self.size.x - w - (style.padding.x * 3)
    end
    renderer.draw_rect(
      x + style.padding.x,
      self.position.y - h - (style.padding.y * 2),
      w + (style.padding.x * 2),
      h + (style.padding.y * 2),
      style.background3
    )
    renderer.draw_text(
      style.font, text,
      x + (style.padding.x * 2),
      self.position.y - h - style.padding.y,
      style.text
    )
  end)
end

function StatusView:draw()
  if not self.visible and self.size.y <= 0 then return end
  self:draw_background(style.background2)

  if self.message and system.get_time() <= self.message_timeout then
    self:draw_items(self.message, false, 0, self.size.y)
  else
    if self.tooltip_mode then self:draw_items(self.tooltip) end
    if #self.active_items > 0 then
      core.push_clip_rect(0, self.position.y, self.left_width + style.padding.x, self.size.y)
      for _, item in ipairs(self.active_items) do
        local item_x = self.left_xoffset + item.x + style.padding.x
        local hovered = self.hovered_item == item
        local item_bg = hovered and item.background_color_hover or item.background_color
        if item.alignment == StatusView.Item.LEFT and not self.tooltip_mode then
          if type(item_bg) == "table" then
            renderer.draw_rect(item_x, self.position.y, item.w, self.size.y, item_bg)
          end
          if item.on_draw then
            core.push_clip_rect(item_x, self.position.y, item.w, self.size.y)
            item.on_draw(item_x, self.position.y, self.size.y, hovered)
            core.pop_clip_rect()
          else
            self:draw_items(item.cached_item, false, item_x - style.padding.x)
          end
        end
      end
      core.pop_clip_rect()

      core.push_clip_rect(
        self.size.x - (self.right_width + style.padding.x), self.position.y,
        self.right_width + style.padding.x, self.size.y
      )
      for _, item in ipairs(self.active_items) do
        local item_x = self.right_xoffset + item.x + style.padding.x
        local hovered = self.hovered_item == item
        local item_bg = hovered and item.background_color_hover or item.background_color
        if item.alignment == StatusView.Item.RIGHT then
          if type(item_bg) == "table" then
            renderer.draw_rect(item_x, self.position.y, item.w, self.size.y, item_bg)
          end
          if item.on_draw then
            core.push_clip_rect(item_x, self.position.y, item.w, self.size.y)
            item.on_draw(item_x, self.position.y, self.size.y, hovered)
            core.pop_clip_rect()
          else
            self:draw_items(item.cached_item, false, item_x - style.padding.x)
          end
        end
      end
      core.pop_clip_rect()

      if self.hovered_item.tooltip ~= "" and self.hovered_item.active then
        self:draw_item_tooltip(self.hovered_item)
      end
    end
  end
end

-- ── Default item registration (stays in Lua: callbacks close over Lua globals) ─

function StatusView:register_docview_items()
  if self:get_item("doc:file") then return end

  local DocView = require "core.docview"
  local CommandView = require "core.commandview"
  local function predicate_docview()
    return core.active_view:is(DocView) and not core.active_view:is(CommandView)
  end

  self:add_item({
    predicate = predicate_docview,
    name = "doc:file",
    alignment = StatusView.Item.LEFT,
    get_item = function()
      local dv = core.active_view
      return {
        dv.doc:is_dirty() and style.accent or style.text, style.icon_font, "f",
        style.dim, style.font, self.separator2, style.text,
        dv.doc.filename and style.text or style.dim, common.home_encode(dv.doc:get_name())
      }
    end
  })

  self:add_item({
    predicate = predicate_docview,
    name = "doc:position",
    alignment = StatusView.Item.LEFT,
    get_item = function()
      local dv = core.active_view
      local line, col = dv.doc:get_selection()
      local _, indent_size = dv.doc:get_indent_info()
      local ntabs, last_idx = 0, 0
      while last_idx < col do
        local s, e = string.find(dv.doc.lines[line], "\t", last_idx, true)
        if s and s < col then
          ntabs = ntabs + 1
          last_idx = e + 1
        else break end
      end
      col = col + ntabs * (indent_size - 1)
      return {
        style.text, line, ":",
        col > config.line_limit and style.accent or style.text, col,
        style.text
      }
    end,
    command = "doc:go-to-line",
    tooltip = "line : column"
  })

  self:add_item({
    predicate = predicate_docview,
    name = "doc:position-percent",
    alignment = StatusView.Item.LEFT,
    get_item = function()
      local dv = core.active_view
      local line = dv.doc:get_selection()
      return { string.format("%.f%%", line / #dv.doc.lines * 100) }
    end,
    tooltip = "caret position"
  })

  self:add_item({
    predicate = predicate_docview,
    name = "doc:selections",
    alignment = StatusView.Item.LEFT,
    get_item = function()
      local dv = core.active_view
      local nsel = math.floor(#dv.doc.selections / 4)
      if nsel > 1 then return { style.text, nsel, " selections" } end
      return {}
    end
  })

  self:add_item({
    predicate = predicate_docview,
    name = "doc:indentation",
    alignment = StatusView.Item.RIGHT,
    get_item = function()
      local dv = core.active_view
      local indent_type, indent_size, indent_confirmed = dv.doc:get_indent_info()
      local indent_label = (indent_type == "hard") and "tabs: " or "spaces: "
      return {
        style.text, indent_label, indent_size,
        indent_confirmed and "" or "*"
      }
    end,
    command = function(button, x, y)
      if button == "left" then
        command.perform "indent:set-file-indent-size"
      elseif button == "right" then
        command.perform "indent:set-file-indent-type"
      end
    end,
    separator = self.separator2
  })

  self:add_item({
    predicate = predicate_docview,
    name = "doc:lines",
    alignment = StatusView.Item.RIGHT,
    get_item = function()
      local dv = core.active_view
      return { style.text, #dv.doc.lines, " lines" }
    end,
    separator = self.separator2
  })

  self:add_item({
    predicate = predicate_docview,
    name = "doc:line-ending",
    alignment = StatusView.Item.RIGHT,
    get_item = function()
      local dv = core.active_view
      return { style.text, dv.doc.crlf and "CRLF" or "LF" }
    end,
    command = "doc:toggle-line-ending"
  })

  self:add_item {
    predicate = predicate_docview,
    name = "doc:overwrite-mode",
    alignment = StatusView.Item.RIGHT,
    get_item = function()
      return { style.text, core.active_view.doc.overwrite and "OVR" or "INS" }
    end,
    command = "doc:toggle-overwrite",
    separator = StatusView.separator2
  }

  self:add_item({
    predicate = predicate_docview,
    name = "doc:mode",
    alignment = StatusView.Item.RIGHT,
    get_item = function()
      local dv = core.active_view
      if not dv.doc.large_file_mode and not dv.doc.read_only then return {} end
      local items = {}
      if dv.doc.large_file_mode then
        items[#items + 1] = style.warn
        items[#items + 1] = "LARGE"
      end
      if dv.doc.read_only then
        if #items > 0 then
          items[#items + 1] = style.dim
          items[#items + 1] = " "
        end
        items[#items + 1] = style.accent
        items[#items + 1] = "RO"
      end
      return items
    end,
    separator = StatusView.separator2
  })
end

function StatusView:register_command_items()
  if self:get_item("command:files") then return end
  self:add_item({
    predicate = "core.commandview",
    name = "command:files",
    alignment = StatusView.Item.RIGHT,
    get_item = function() return { style.icon_font, "g" } end
  })
end

return StatusView
"#;

// ── Rust native helpers ──────────────────────────────────────────────────────

fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

/// Compute normalised insertion index matching the Lua normalize_position local.
fn normalize_position(items: &LuaTable, position: i64, alignment: i64) -> LuaResult<i64> {
    let mut left_count = 0i64;
    let mut right_count = 0i64;
    for pair in items.clone().sequence_values::<LuaTable>() {
        let item = pair?;
        let align: i64 = item.get::<Option<i64>>("alignment")?.unwrap_or(1);
        if align == 1 {
            left_count += 1;
        } else {
            right_count += 1;
        }
    }
    let (offset, items_count) = if alignment == 2 {
        (left_count, right_count)
    } else {
        (0i64, left_count)
    };
    let total = left_count + right_count;
    let mut pos = if position == 0 {
        offset + 1
    } else if position < 0 {
        offset + items_count + position + 2
    } else {
        offset + position
    };
    if pos < 1 {
        pos = offset + 1;
    }
    if pos > total + 1 {
        pos = offset + items_count + 1;
    }
    Ok(pos.max(1))
}

/// Initialise all mutable state fields on the StatusView Lua table.
fn init(lua: &Lua, self_table: LuaTable) -> LuaResult<()> {
    self_table.set("message_timeout", 0.0f64)?;
    self_table.set("message", lua.create_table()?)?;
    self_table.set("tooltip_mode", false)?;
    self_table.set("tooltip", lua.create_table()?)?;
    self_table.set("items", lua.create_table()?)?;
    self_table.set("active_items", lua.create_table()?)?;
    self_table.set("hovered_item", lua.create_table()?)?;
    let pointer = lua.create_table()?;
    pointer.set("x", 0.0f64)?;
    pointer.set("y", 0.0f64)?;
    self_table.set("pointer", pointer)?;
    self_table.set("left_width", 0.0f64)?;
    self_table.set("right_width", 0.0f64)?;
    self_table.set("r_left_width", 0.0f64)?;
    self_table.set("r_right_width", 0.0f64)?;
    self_table.set("left_xoffset", 0.0f64)?;
    self_table.set("right_xoffset", 0.0f64)?;
    self_table.set("dragged_panel", "")?;
    self_table.set("hovered_panel", "")?;
    self_table.set("hide_messages", false)?;
    self_table.set("visible", true)?;
    self_table.set("_separator_width", LuaValue::Nil)?;
    self_table.set("_separator2_width", LuaValue::Nil)?;
    Ok(())
}

/// Find the first item whose `name` field matches, return it or nil.
fn get_item(_lua: &Lua, (self_table, name): (LuaTable, String)) -> LuaResult<LuaValue> {
    let items: LuaTable = self_table.get("items")?;
    for item in items.sequence_values::<LuaTable>() {
        let item = item?;
        let item_name: String = item.get::<Option<String>>("name")?.unwrap_or_default();
        if item_name == name {
            return Ok(LuaValue::Table(item));
        }
    }
    Ok(LuaValue::Nil)
}

/// Remove the item with the given name from self.items; return it or nil.
fn remove_item(_lua: &Lua, (self_table, name): (LuaTable, String)) -> LuaResult<LuaValue> {
    let items: LuaTable = self_table.get("items")?;
    let len = items.raw_len();
    for i in 1..=len {
        let item: LuaTable = items.raw_get(i as i64)?;
        let item_name: String = item.get::<Option<String>>("name")?.unwrap_or_default();
        if item_name == name {
            // Shift items down manually (Lua table.remove semantics).
            for j in i..len {
                let next: LuaValue = items.raw_get((j + 1) as i64)?;
                items.raw_set(j as i64, next)?;
            }
            items.raw_set(len as i64, LuaValue::Nil)?;
            return Ok(LuaValue::Table(item));
        }
    }
    Ok(LuaValue::Nil)
}

/// Move a named item to a new position and optional alignment.
fn move_item(
    lua: &Lua,
    (self_table, name, position, alignment): (LuaTable, String, i64, Option<i64>),
) -> LuaResult<bool> {
    let removed = remove_item(lua, (self_table.clone(), name))?;
    let item = match removed {
        LuaValue::Table(t) => t,
        _ => return Ok(false),
    };
    if let Some(align) = alignment {
        item.set("alignment", align)?;
    }
    let item_align: i64 = item.get::<Option<i64>>("alignment")?.unwrap_or(1);
    let items: LuaTable = self_table.get("items")?;
    let pos = normalize_position(&items, position, item_align)?;
    // Re-insert: shift items up from pos.
    let len = items.raw_len();
    for i in (pos..=len as i64).rev() {
        let val: LuaValue = items.raw_get(i)?;
        items.raw_set(i + 1, val)?;
    }
    items.raw_set(pos, item)?;
    Ok(true)
}

/// Reorder items so that `names[i]` appears at position `i`.
fn order_items(
    lua: &Lua,
    (self_table, names): (LuaTable, LuaTable),
) -> LuaResult<()> {
    let mut removed: Vec<LuaTable> = Vec::new();
    for name in names.sequence_values::<String>() {
        let name = name?;
        if let LuaValue::Table(item) = remove_item(lua, (self_table.clone(), name))? {
            removed.push(item);
        }
    }
    let items: LuaTable = self_table.get("items")?;
    // Shift existing items to make room at front, then insert removed items.
    let existing_len = items.raw_len();
    let insert_count = removed.len() as i64;
    for i in (1..=existing_len as i64).rev() {
        let val: LuaValue = items.raw_get(i)?;
        items.raw_set(i + insert_count, val)?;
    }
    for (idx, item) in removed.into_iter().enumerate() {
        items.raw_set(idx as i64 + 1, item)?;
    }
    Ok(())
}

/// Return a new table containing only items matching `alignment` (or all if nil).
fn get_items_list(
    lua: &Lua,
    (self_table, alignment): (LuaTable, Option<i64>),
) -> LuaResult<LuaTable> {
    let items: LuaTable = self_table.get("items")?;
    let result = lua.create_table()?;
    if let Some(align) = alignment {
        let mut idx = 1i64;
        for item in items.sequence_values::<LuaTable>() {
            let item = item?;
            let item_align: i64 = item.get::<Option<i64>>("alignment")?.unwrap_or(1);
            if item_align == align {
                result.raw_set(idx, item)?;
                idx += 1;
            }
        }
    } else {
        return Ok(items);
    }
    Ok(result)
}

/// Set the timed message fields; no-op if hide_messages is set.
fn show_message(
    lua: &Lua,
    (self_table, icon, icon_color, text): (LuaTable, String, LuaValue, String),
) -> LuaResult<()> {
    if !self_table.get::<bool>("visible")?
        || self_table.get::<bool>("hide_messages")?
    {
        return Ok(());
    }
    let style = require_table(lua, "core.style")?;
    let system = require_table(lua, "system")?;
    let get_time: LuaFunction = system.get("get_time")?;
    let config = require_table(lua, "core.config")?;
    let message_timeout: f64 = config.get("message_timeout")?;
    let now: f64 = get_time.call(())?;

    let msg = lua.create_table()?;
    msg.raw_set(1, icon_color)?;
    msg.raw_set(2, style.get::<LuaValue>("icon_font")?)?;
    msg.raw_set(3, icon)?;
    msg.raw_set(4, style.get::<LuaValue>("dim")?)?;
    msg.raw_set(5, style.get::<LuaValue>("font")?)?;
    msg.raw_set(6, "   |   ")?; // separator2
    msg.raw_set(7, style.get::<LuaValue>("text")?)?;
    msg.raw_set(8, text)?;
    self_table.set("message", msg)?;
    self_table.set("message_timeout", now + message_timeout)?;
    Ok(())
}

/// Apply panel layout using status_model.fit_panels; write results back to self.
fn apply_panel_layout(
    lua: &Lua,
    (self_table, raw_left, raw_right): (LuaTable, f64, f64),
) -> LuaResult<()> {
    let status_model = require_table(lua, "status_model")?;
    let fit_panels: LuaFunction = status_model.get("fit_panels")?;
    let style = require_table(lua, "core.style")?;
    let padding: LuaTable = style.get("padding")?;
    let padding_x: f64 = padding.get("x")?;
    let size: LuaTable = self_table.get("size")?;
    let total_width: f64 = size.get("x")?;
    let left_xoffset: f64 = self_table.get("left_xoffset")?;
    let right_xoffset: f64 = self_table.get("right_xoffset")?;

    let fit: LuaTable = fit_panels.call((
        total_width, raw_left, raw_right, padding_x, left_xoffset, right_xoffset,
    ))?;
    self_table.set("left_width", fit.get::<f64>("left_width")?)?;
    self_table.set("right_width", fit.get::<f64>("right_width")?)?;
    self_table.set("left_xoffset", fit.get::<f64>("left_offset")?)?;
    self_table.set("right_xoffset", fit.get::<f64>("right_offset")?)?;
    Ok(())
}

/// Update dragged-panel offset via status_model.drag_panel_offset.
fn drag_panel(
    lua: &Lua,
    (self_table, panel, dx): (LuaTable, String, f64),
) -> LuaResult<()> {
    let status_model = require_table(lua, "status_model")?;
    let drag_fn: LuaFunction = status_model.get("drag_panel_offset")?;

    let r_left_width: f64 = self_table.get("r_left_width")?;
    let r_right_width: f64 = self_table.get("r_right_width")?;
    let left_width: f64 = self_table.get("left_width")?;
    let right_width: f64 = self_table.get("right_width")?;

    if panel == "left" && r_left_width > left_width {
        let left_xoffset: f64 = self_table.get("left_xoffset")?;
        let new_offset: f64 =
            drag_fn.call((left_xoffset, r_left_width, left_width, dx))?;
        self_table.set("left_xoffset", new_offset)?;
    } else if panel == "right" && r_right_width > right_width {
        let right_xoffset: f64 = self_table.get("right_xoffset")?;
        let new_offset: f64 =
            drag_fn.call((right_xoffset, r_right_width, right_width, dx))?;
        self_table.set("right_xoffset", new_offset)?;
    }
    Ok(())
}

/// Return "left" or "right" depending on cursor position.
fn get_hovered_panel(
    _lua: &Lua,
    (self_table, x, y): (LuaTable, f64, f64),
) -> LuaResult<&'static str> {
    let position: LuaTable = self_table.get("position")?;
    let pos_y: f64 = position.get("y")?;
    let left_width: f64 = self_table.get("left_width")?;
    let style_res = _lua.globals().get::<Option<LuaTable>>("style");
    let padding_x = style_res
        .ok()
        .flatten()
        .and_then(|s| s.get::<Option<LuaTable>>("padding").ok().flatten())
        .and_then(|p| p.get::<Option<f64>>("x").ok().flatten())
        .unwrap_or(4.0);
    if y >= pos_y && x <= left_width + padding_x {
        Ok("left")
    } else {
        Ok("right")
    }
}

/// Compute the visible area of an item via status_model.item_visible_area.
fn get_item_visible_area(
    lua: &Lua,
    (self_table, item): (LuaTable, LuaTable),
) -> LuaResult<(f64, f64)> {
    let status_model = require_table(lua, "status_model")?;
    let item_visible_fn: LuaFunction = status_model.get("item_visible_area")?;
    let style = require_table(lua, "core.style")?;
    let padding: LuaTable = style.get("padding")?;
    let padding_x: f64 = padding.get("x")?;

    let alignment: i64 = item.get::<Option<i64>>("alignment")?.unwrap_or(1);
    let is_left = alignment == 1;

    let left_width: f64 = self_table.get("left_width")?;
    let right_width: f64 = self_table.get("right_width")?;
    let size: LuaTable = self_table.get("size")?;
    let size_x: f64 = size.get("x")?;

    let panel_width = if is_left {
        left_width
    } else {
        size_x - right_width
    };
    let item_ox: f64 = if is_left {
        self_table.get("left_xoffset")?
    } else {
        self_table.get("right_xoffset")?
    };
    let item_x: f64 = item.get("x")?;
    let item_w: f64 = item.get("w")?;

    item_visible_fn.call((is_left, panel_width, padding_x, item_ox, item_x, item_w))
}

fn on_mouse_pressed(
    lua: &Lua,
    (self_table, button, x, y, clicks): (LuaTable, String, f64, f64, i64),
) -> LuaResult<bool> {
    let visible: bool = self_table.get("visible")?;
    if !visible {
        return Ok(false);
    }
    let core = require_table(lua, "core")?;
    let set_active: LuaFunction = core.get("set_active_view")?;
    let last_active: LuaValue = core.get("last_active_view")?;
    set_active.call::<()>(last_active)?;

    let system = require_table(lua, "system")?;
    let get_time: LuaFunction = system.get("get_time")?;
    let now: f64 = get_time.call(())?;
    let message_timeout: f64 = self_table.get("message_timeout")?;

    if now < message_timeout {
        let log_view_mod = require_table(lua, "core.logview")?;
        let active_view: LuaTable = core.get("active_view")?;
        let is_fn: LuaFunction = active_view.get("is")?;
        let is_log: bool = is_fn.call((active_view.clone(), log_view_mod))?;
        if !is_log {
            let command = require_table(lua, "core.command")?;
            let perform: LuaFunction = command.get("perform")?;
            perform.call::<()>("core:open-log")?;
        }
    } else {
        let position: LuaTable = self_table.get("position")?;
        let pos_y: f64 = position.get("y")?;
        if y >= pos_y && button == "left" && clicks == 1 {
            position.set("dx", x)?;
            let r_left: f64 = self_table.get("r_left_width")?;
            let r_right: f64 = self_table.get("r_right_width")?;
            let left_w: f64 = self_table.get("left_width")?;
            let right_w: f64 = self_table.get("right_width")?;
            if r_left > left_w || r_right > right_w {
                let panel = get_hovered_panel(lua, (self_table.clone(), x, y))?;
                self_table.set("dragged_panel", panel)?;
                self_table.set("cursor", "hand")?;
            }
        }
    }
    Ok(true)
}

fn on_mouse_moved(
    lua: &Lua,
    (self_table, x, y, dx, _dy): (LuaTable, f64, f64, f64, f64),
) -> LuaResult<()> {
    let visible: bool = self_table.get("visible")?;
    if !visible {
        return Ok(());
    }

    let panel = get_hovered_panel(lua, (self_table.clone(), x, y))?;
    self_table.set("hovered_panel", panel)?;

    let dragged_panel: String = self_table.get("dragged_panel")?;
    if !dragged_panel.is_empty() {
        drag_panel(lua, (self_table, dragged_panel, dx))?;
        return Ok(());
    }

    let position: LuaTable = self_table.get("position")?;
    let pos_y: f64 = position.get("y")?;
    let system = require_table(lua, "system")?;
    let get_time: LuaFunction = system.get("get_time")?;
    let now: f64 = get_time.call(())?;
    let message_timeout: f64 = self_table.get("message_timeout")?;

    if y < pos_y || now <= message_timeout {
        self_table.set("cursor", "arrow")?;
        self_table.set("hovered_item", lua.create_table()?)?;
        return Ok(());
    }

    let active_items: LuaTable = self_table.get("active_items")?;
    for item in active_items.sequence_values::<LuaTable>() {
        let item = item?;
        let item_visible: bool = item.get::<Option<bool>>("visible")?.unwrap_or(true);
        let item_active: bool = item.get::<Option<bool>>("active")?.unwrap_or(false);
        let has_command: bool = item.get::<Option<LuaValue>>("command")?.is_some();
        let has_click: bool = item.get::<Option<LuaFunction>>("on_click")?.is_some();
        let tooltip: String = item.get::<Option<String>>("tooltip")?.unwrap_or_default();

        if item_visible && item_active && (has_command || has_click || !tooltip.is_empty()) {
            let (item_x, item_w) = get_item_visible_area(lua, (self_table.clone(), item.clone()))?;
            if x > item_x && (item_x + item_w) > x {
                let pointer: LuaTable = self_table.get("pointer")?;
                pointer.set("x", x)?;
                pointer.set("y", y)?;
                let hovered: LuaValue = self_table.get("hovered_item")?;
                if hovered != LuaValue::Table(item.clone()) {
                    self_table.set("hovered_item", item.clone())?;
                }
                if has_command || has_click {
                    self_table.set("cursor", "hand")?;
                }
                return Ok(());
            }
        }
    }
    self_table.set("cursor", "arrow")?;
    self_table.set("hovered_item", lua.create_table()?)?;
    Ok(())
}

fn on_mouse_released(
    lua: &Lua,
    (self_table, button, x, _y): (LuaTable, String, f64, f64),
) -> LuaResult<()> {
    let visible: bool = self_table.get("visible")?;
    if !visible {
        return Ok(());
    }

    let dragged_panel: String = self_table.get("dragged_panel")?;
    if !dragged_panel.is_empty() {
        self_table.set("dragged_panel", "")?;
        self_table.set("cursor", "arrow")?;
        let position: LuaTable = self_table.get("position")?;
        let drag_start: f64 = position.get::<Option<f64>>("dx")?.unwrap_or(x);
        if (drag_start - x).abs() > f64::EPSILON {
            return Ok(());
        }
    }

    let position: LuaTable = self_table.get("position")?;
    let pos_y: f64 = position.get("y")?;
    if _y < pos_y {
        return Ok(());
    }
    let hovered_item: LuaTable = self_table.get("hovered_item")?;
    let is_active: bool = hovered_item.get::<Option<bool>>("active")?.unwrap_or(false);
    if !is_active {
        return Ok(());
    }

    let (item_x, item_w) =
        get_item_visible_area(lua, (self_table.clone(), hovered_item.clone()))?;
    if x > item_x && (item_x + item_w) > x {
        if let Some(cmd) = hovered_item.get::<Option<String>>("command")? {
            let command = require_table(lua, "core.command")?;
            let perform: LuaFunction = command.get("perform")?;
            perform.call::<()>(cmd)?;
        } else if let Some(on_click) = hovered_item.get::<Option<LuaFunction>>("on_click")? {
            on_click.call::<()>((button, x, _y))?;
        }
    }
    Ok(())
}

fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    let m = lua.create_table()?;
    m.set("init", lua.create_function(|lua, self_table: LuaTable| init(lua, self_table))?)?;
    m.set(
        "normalize_position",
        lua.create_function(|_, (items, pos, align): (LuaTable, i64, i64)| {
            normalize_position(&items, pos, align)
        })?,
    )?;
    m.set("get_item", lua.create_function(get_item)?)?;
    m.set("remove_item", lua.create_function(remove_item)?)?;
    m.set("move_item", lua.create_function(move_item)?)?;
    m.set("order_items", lua.create_function(order_items)?)?;
    m.set("get_items_list", lua.create_function(get_items_list)?)?;
    m.set("show_message", lua.create_function(show_message)?)?;
    m.set("apply_panel_layout", lua.create_function(apply_panel_layout)?)?;
    m.set("drag_panel", lua.create_function(drag_panel)?)?;
    m.set("get_hovered_panel", lua.create_function(get_hovered_panel)?)?;
    m.set("get_item_visible_area", lua.create_function(get_item_visible_area)?)?;
    m.set("on_mouse_pressed", lua.create_function(on_mouse_pressed)?)?;
    m.set("on_mouse_moved", lua.create_function(on_mouse_moved)?)?;
    m.set("on_mouse_released", lua.create_function(on_mouse_released)?)?;
    Ok(m)
}

pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let package: LuaTable = lua.globals().get("package")?;
    let preload: LuaTable = package.get("preload")?;
    let native_key = lua.create_registry_value(make_module(lua)?)?;
    preload.set(
        "statusview_native",
        lua.create_function(move |lua, ()| lua.registry_value::<LuaTable>(&native_key))?,
    )?;
    preload.set(
        "core.statusview",
        lua.create_function(|lua, ()| {
            lua.load(BOOTSTRAP)
                .set_name("core.statusview")
                .eval::<LuaValue>()
        })?,
    )?;
    Ok(())
}
