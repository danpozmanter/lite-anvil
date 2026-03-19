local core = require "core"
local common = require "core.common"
local style = require "core.style"
local View = require "core.view"

local icon_colors = {
  bg = { common.color "#2e2e32ff" },
  color6 = { common.color "#e1e1e6ff" },
  color7 = { common.color "#ffa94dff" },
  color8 = { common.color "#93ddfaff" },
  color9 = { common.color "#f7c95cff" }
}

local restore_command = {
  symbol = "w", action = function() system.set_window_mode(core.window, "normal") end
}

local maximize_command = {
  symbol = "W", action = function() system.set_window_mode(core.window, "maximized") end
}

local title_commands = {
  { symbol = "_", action = function() system.set_window_mode(core.window, "minimized") end },
  maximize_command,
  { symbol = "X", action = function() core.quit() end },
}

---@class core.titleview : core.view
---@field super core.view
local TitleView = View:extend()

function TitleView:__tostring() return "TitleView" end

local function title_view_height()
  return style.font:get_height() + style.padding.y * 2
end

local function title_separator_inset()
  return math.max(10, style.padding.x - 2)
end

function TitleView:new()
  TitleView.super.new(self)
  self.visible = true
  self._control_metrics = nil
end

function TitleView:get_control_metrics()
  local metrics = self._control_metrics
  if metrics and metrics.font == style.icon_font then
    return metrics
  end
  local icon_w = style.icon_font:get_width("_")
  metrics = {
    font = style.icon_font,
    width = icon_w,
    height = style.icon_font:get_height(),
    spacing = math.max(style.padding.x * 0.75, math.floor(icon_w * 0.7)),
    hit_width = math.max(icon_w + style.padding.x, style.font:get_height()),
  }
  self._control_metrics = metrics
  return metrics
end

function TitleView:configure_hit_test(borderless)
  if borderless then
    local title_height = title_view_height()
    local metrics = self:get_control_metrics()
    local controls_width = metrics.hit_width * #title_commands + metrics.spacing
    system.set_window_hit_test(core.window, title_height, controls_width, metrics.spacing)
  else
    system.set_window_hit_test(core.window)
  end
end

function TitleView:on_scale_change()
  self._control_metrics = nil
  self:configure_hit_test(self.visible)
end

function TitleView:update()
  self.size.y = self.visible and title_view_height() or 0
  title_commands[2] = core.window_mode == "maximized" and restore_command or maximize_command
  TitleView.super.update(self)
end


function TitleView:draw_window_title()
  local h = style.font:get_height()
  local ox, oy = self:get_content_offset()
  local color = style.text
  local metrics = self:get_control_metrics()
  local controls_width = metrics.hit_width * #title_commands + metrics.spacing
  local x, y = ox + title_separator_inset(), oy + style.padding.y
  common.draw_text(style.icon_font, icon_colors.bg, "5", nil, x, y, 0, h)
  common.draw_text(style.icon_font, icon_colors.color6, "6", nil, x, y, 0, h)
  common.draw_text(style.icon_font, icon_colors.color7, "7", nil, x, y, 0, h)
  common.draw_text(style.icon_font, icon_colors.color8, "8", nil, x, y, 0, h)
  x = common.draw_text(style.icon_font, icon_colors.color9, "9 ", nil, x, y, 0, h)
  local title = core.compose_window_title(core.window_title)
  local title_width = math.max(0, self.size.x - controls_width - x - title_separator_inset())
  core.push_clip_rect(x, self.position.y, title_width, self.size.y)
  common.draw_text(style.font, color, title, nil, x, y, title_width, h)
  core.pop_clip_rect()
end

function TitleView:each_control_item()
  local metrics = self:get_control_metrics()
  local ox, oy = self:get_content_offset()
  ox = ox + self.size.x - title_separator_inset()
  local i, n = 0, #title_commands
  local iter = function()
    i = i + 1
    if i <= n then
      local dx = -(metrics.hit_width * (n - i + 1))
      local x = ox + dx
      local y = oy + style.padding.y
      return title_commands[i], x, y, metrics.hit_width, metrics.height, metrics
    end
  end
  return iter
end


function TitleView:draw_window_controls()
  for item, x, y, w, h in self:each_control_item() do
    local color = item == self.hovered_item and style.text or style.dim
    if item == self.hovered_item then
      local hover_bg = { table.unpack(style.line_highlight) }
      hover_bg[4] = 140
      renderer.draw_rect(x, self.position.y, w, self.size.y, hover_bg)
    end
    common.draw_text(style.icon_font, color, item.symbol, "center", x, y, w, h)
  end
end


function TitleView:on_mouse_pressed(button, x, y, clicks)
  local caught = TitleView.super.on_mouse_pressed(self, button, x, y, clicks)
  if caught then return end
  core.set_active_view(core.last_active_view)
  if self.hovered_item then
    self.hovered_item.action()
  end
end


function TitleView:on_mouse_left()
  TitleView.super.on_mouse_left(self)
  self.hovered_item = nil
end


function TitleView:on_mouse_moved(px, py, ...)
  if self.size.y == 0 then return end
  TitleView.super.on_mouse_moved(self, px, py, ...)
  self.hovered_item = nil
  for item, x, y, w, h in self:each_control_item() do
    if px > x and py > y and px <= x + w and py <= y + h then
      self.hovered_item = item
      return
    end
  end
end


function TitleView:draw()
  self:draw_background(style.background2)
  self:draw_window_title()
  self:draw_window_controls()
  renderer.draw_rect(
    self.position.x + title_separator_inset(),
    self.position.y + self.size.y - style.divider_size,
    math.max(0, self.size.x - title_separator_inset() * 2),
    style.divider_size,
    style.divider
  )
end

return TitleView
