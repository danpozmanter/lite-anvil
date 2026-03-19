-- mod-version:4
local syntax = require "core.syntax"
local style = require "core.style"
local core = require "core"

syntax.add_from_asset("md")

-- Preserve the existing markdown emphasis styling behavior while the
-- syntax definition itself stays declarative.
core.add_thread(function()
  local custom_fonts = { bold = { font = nil, color = nil }, italic = {}, bold_italic = {} }
  local initial_color
  local last_code_font

  local function set_font(attr)
    local attributes = {}
    if attr ~= "bold_italic" then
      attributes[attr] = true
    else
      attributes.bold = true
      attributes.italic = true
    end
    local font = style.code_font:copy(style.code_font:get_size(), attributes)
    custom_fonts[attr].font = font
    style.syntax_fonts["markdown_" .. attr] = font
  end

  local function set_color(attr)
    custom_fonts[attr].color = style.syntax.keyword2
    style.syntax["markdown_" .. attr] = style.syntax.keyword2
  end

  for attr, _ in pairs(custom_fonts) do
    if not style.syntax_fonts["markdown_" .. attr] then
      set_font(attr)
    end
    if not style.syntax["markdown_" .. attr] then
      set_color(attr)
    end
  end

  while true do
    if last_code_font ~= style.code_font then
      last_code_font = style.code_font
      for attr, _ in pairs(custom_fonts) do
        if style.syntax_fonts["markdown_" .. attr] == custom_fonts[attr].font then
          set_font(attr)
        end
      end
    end

    if initial_color ~= style.syntax.keyword2 then
      initial_color = style.syntax.keyword2
      for attr, _ in pairs(custom_fonts) do
        if style.syntax["markdown_" .. attr] == custom_fonts[attr].color then
          set_color(attr)
        end
      end
    end

    coroutine.yield(1)
  end
end)
