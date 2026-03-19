-- mod-version:4
local common = require "core.common"
local command = require "core.command"
local config = require "core.config"
local style = require "core.style"
local DocView = require "core.docview"

config.plugins.lineguide = common.merge({
  enabled = false,
  width = 2,
  rulers = { config.line_limit },
  use_custom_color = false,
  custom_color = style.selection,
  config_spec = {
    name = "Line Guide",
    {
      label = "Enabled",
      description = "Disable or enable drawing of the line guide.",
      path = "enabled",
      type = "toggle",
      default = true
    },
    {
      label = "Width",
      description = "Width in pixels of the line guide.",
      path = "width",
      type = "number",
      default = 2,
      min = 1
    },
    {
      label = "Ruler Positions",
      description = "The different column numbers for the line guides to draw.",
      path = "rulers",
      type = "list_strings",
      default = { tostring(config.line_limit) or "80" },
      get_value = function(rulers)
        if type(rulers) == "table" then
          local new_rulers = {}
          for _, ruler in ipairs(rulers) do
            new_rulers[#new_rulers + 1] = tostring(ruler)
          end
          return new_rulers
        end
        return { tostring(config.line_limit) }
      end,
      set_value = function(rulers)
        local new_rulers = {}
        for _, ruler in ipairs(rulers) do
          local number = tonumber(ruler)
          if number then
            new_rulers[#new_rulers + 1] = number
          end
        end
        if #new_rulers == 0 then
          new_rulers[#new_rulers + 1] = config.line_limit
        end
        return new_rulers
      end
    },
    {
      label = "Use Custom Color",
      description = "Enable the utilization of a custom line color.",
      path = "use_custom_color",
      type = "toggle",
      default = false
    },
    {
      label = "Custom Color",
      description = "Applied when the above toggle is enabled.",
      path = "custom_color",
      type = "color",
      default = style.selection
    },
  }
}, config.plugins.lineguide)

local function get_ruler(v)
  if type(v) == "number" then
    return { columns = v }
  elseif type(v) == "table" then
    return v
  end
end

local old_draw_overlay = DocView.draw_overlay
function DocView:draw_overlay(...)
  if type(config.plugins.lineguide) == "table" and config.plugins.lineguide.enabled and self:is(DocView) then
    local conf = config.plugins.lineguide
    local line_x = self:get_line_screen_position(1)
    local character_width = self:get_font():get_width("n")
    local ruler_width = config.plugins.lineguide.width
    local ruler_color = conf.use_custom_color and conf.custom_color or (style.guide or style.selection)
    for _, v in ipairs(config.plugins.lineguide.rulers) do
      local ruler = get_ruler(v)
      if ruler then
        local x = line_x + (character_width * ruler.columns)
        renderer.draw_rect(x, self.position.y, ruler_width, self.size.y, ruler.color or ruler_color)
      end
    end
  end
  old_draw_overlay(self, ...)
end

command.add(nil, {
  ["lineguide:toggle"] = function()
    config.plugins.lineguide.enabled = not config.plugins.lineguide.enabled
  end
})
