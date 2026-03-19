local core = require "core"
local common = require "core.common"
local command = require "core.command"
local keymap = require "core.keymap"
local style = require "core.style"
local View = require "core.view"

local border_width = 1
local divider_width = 1
local divider_padding = 5
local DIVIDER = {}

---@class core.contextmenu.item
---@field text string
---@field info string?
---@field command string|fun(...)

---@class core.contextmenu.contextdetails
---@field items core.contextmenu.item[]
---@field x integer?
---@field y integer?

---@class core.contextmenu : core.view
---@field visible boolean
---@field selected number
---@field position core.view.position
---@field current_scale number
local ContextMenu = View:extend()

function ContextMenu:__tostring() return "ContextMenu" end

ContextMenu.DIVIDER = DIVIDER

function ContextMenu:new()
  self.visible = false
  self.selected = -1
  self.height = 0
  self.position = { x = 0, y = 0 }
  self.current_scale = SCALE
end

local function get_item_size(item)
  local lw, lh
  if item == DIVIDER then
    lw = 0
    lh = divider_width + divider_padding * SCALE * 2
  else
    lw = style.font:get_width(item.text)
    if item.info then
      lw = lw + style.padding.x + style.font:get_width(item.info)
    end
    lh = style.font:get_height() + style.padding.y
  end
  return lw, lh
end

local function update_items_size(items, update_binding)
  local width, height = 0, 0
  for _, item in ipairs(items) do
    if update_binding and item ~= DIVIDER and type(item.command) == "string" then
      item.info = keymap.get_binding_display(item.command)
    end
    local lw, lh = get_item_size(item)
    item.cm_width = lw
    item.cm_height = lh
    item.cm_y = height
    width = math.max(width, lw)
    height = height + lh
  end
  width = width + style.padding.x * 2
  items.width, items.height = width, height
end

function ContextMenu:show(x, y, items, ...)
  local items_list = { width = 0, height = 0, arguments = { ... } }
  for _, item in ipairs(items) do
    if item and (
      not item.command
      or type(item.command) ~= "string"
      or command.is_valid(item.command, ...)
    ) then
      table.insert(items_list, item)
    end
  end

  if #items_list > 0 then
    self.items = items_list
    update_items_size(self.items, true)
    local w, h = self.items.width, self.items.height
    x = common.clamp(x, 0, core.root_view.size.x - w - style.padding.x)
    y = common.clamp(y, 0, core.root_view.size.y - h)

    self.position.x, self.position.y = x, y
    self.visible = true
    core.set_active_view(self)
    core.request_cursor("arrow")
    return true
  end
  return false
end

function ContextMenu:hide()
  self.visible = false
  self.items = nil
  self.selected = -1
  self.height = 0
  if core.active_view == self then
    core.set_active_view(core.last_active_view)
  end
  core.request_cursor(core.active_view.cursor)
end

function ContextMenu:each_item()
  local x, y, w = self.position.x, self.position.y, self.items.width
  return coroutine.wrap(function()
    for i, item in ipairs(self.items) do
      local item_y = y + (item.cm_y or 0)
      local lh = item.cm_height or select(2, get_item_size(item))
      if item_y - y > self.height then break end
      coroutine.yield(i, item, x, item_y, w, lh)
    end
  end)
end

function ContextMenu:on_mouse_pressed(button, x, y)
  if not self.visible then return false end
  if button == "left"
      and x >= self.position.x and y >= self.position.y
      and x < self.position.x + self.items.width and y < self.position.y + self.height then
    local item = self:get_item_selected()
    if not item or not item.command then return true end
    if core.active_view == self then
      core.set_active_view(core.last_active_view)
    end
    self:on_selected(item)
  end
  self:hide()
  return true
end

function ContextMenu:on_mouse_released(button, x, y)
  if not self.visible then return false end
  return true
end

function ContextMenu:on_mouse_moved(px, py)
  if not self.visible then return false end

  self.selected = -1
  for i, _, x, y, w, h in self:each_item() do
    if px > x and px <= x + w and py > y and py <= y + h then
      self.selected = i
      core.request_cursor("arrow")
      break
    end
  end
  return true
end

function ContextMenu:on_selected(item)
  if type(item.command) == "string" then
    command.perform(item.command, table.unpack(self.items.arguments))
  else
    item.command(table.unpack(self.items.arguments))
  end
end

local function change_value(value, change)
  return value + change
end

function ContextMenu:focus_previous()
  self.selected = (self.selected == -1 or self.selected == 1) and #self.items or change_value(self.selected, -1)
  if self:get_item_selected() == DIVIDER then
    self.selected = change_value(self.selected, -1)
  end
end

function ContextMenu:focus_next()
  self.selected = (self.selected == -1 or self.selected == #self.items) and 1 or change_value(self.selected, 1)
  if self:get_item_selected() == DIVIDER then
    self.selected = change_value(self.selected, 1)
  end
end

function ContextMenu:get_item_selected()
  return (self.items or {})[self.selected]
end

ContextMenu.move_towards = View.move_towards

function ContextMenu:update()
  if self.visible then
    self:move_towards("height", self.items.height, nil, "contextmenu")
  end
end

function ContextMenu:draw()
  if not self.visible then return end
  if self.current_scale ~= SCALE then
    update_items_size(self.items)
    self.current_scale = SCALE
  end
  if not self.items then return end
  local bx, by, bw, bh = self.position.x, self.position.y, self.items.width, self.height

  renderer.draw_rect(
    bx - border_width,
    by - border_width,
    bw + (border_width * 2),
    bh + (border_width * 2),
    style.divider
  )
  renderer.draw_rect(bx, by, bw, bh, style.background3)

  for i, item, x, y, w, h in self:each_item() do
    if item == DIVIDER then
      renderer.draw_rect(x, y + divider_padding * SCALE, w, divider_width, style.divider)
    else
      if i == self.selected then
        renderer.draw_rect(x, y, w, h, style.selection)
      end

      common.draw_text(style.font, style.text, item.text, "left", x + style.padding.x, y, w, h)
      if item.info then
        common.draw_text(style.font, style.dim, item.info, "right", x, y, w - style.padding.x, h)
      end
    end
  end
end

return ContextMenu
