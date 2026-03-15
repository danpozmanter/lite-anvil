-- mod-version:4.0.0
local core = require "core"
local command = require "core.command"
local common = require "core.common"
local config = require "core.config"
local storage = require "core.storage"
local style = require "core.style"

local theme_map = {
  dark = "dark_default",
  light = "light_default",
}

config.plugins.theme_toggle = common.merge({
  mode = "dark",
}, config.plugins.theme_toggle)

local function current_mode()
  return config.plugins.theme_toggle.mode == "light" and "light" or "dark"
end

local function apply_mode(mode)
  mode = mode == "light" and "light" or "dark"
  config.plugins.theme_toggle.mode = mode
  config.theme = theme_map[mode]
  style.apply_theme()
  storage.save("theme_toggle", "mode", mode)
  core.redraw = true
end

local saved_mode = storage.load("theme_toggle", "mode")
if saved_mode == "light" or saved_mode == "dark" then
  config.plugins.theme_toggle.mode = saved_mode
  config.theme = theme_map[saved_mode]
  style.apply_theme()
elseif config.theme ~= theme_map.light and config.theme ~= theme_map.dark then
  apply_mode(current_mode())
elseif config.theme == theme_map.light then
  config.plugins.theme_toggle.mode = "light"
else
  config.plugins.theme_toggle.mode = "dark"
end

command.add(nil, {
  ["theme:toggle-mode"] = function()
    apply_mode(current_mode() == "dark" and "light" or "dark")
  end,
})

core.status_view:add_item({
  name = "theme:mode",
  alignment = core.status_view.Item.RIGHT,
  position = 1,
  get_item = function()
    local mode = current_mode()
    local glyph = mode == "dark" and "o" or "*"
    local color = mode == "dark" and style.text or (style.warn or style.text)
    return {
      style.font, color, glyph,
      style.text, " ",
    }
  end,
  command = "theme:toggle-mode",
  tooltip = "Toggle light and dark mode",
  separator = core.status_view.separator2,
})

return {
  apply_mode = apply_mode,
}
