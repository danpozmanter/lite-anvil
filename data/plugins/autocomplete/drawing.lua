-- Drawing functions for the autocomplete suggestions box.
-- Functions that need suggestion state accept a `ctx` table with fields:
-- suggestions, suggestions_idx, suggestions_offset, partial, icons.

local core   = require "core"
local common = require "core.common"
local config = require "core.config"
local style  = require "core.style"

local M = {}

local last_max_width = 0
local previous_scale = SCALE
local desc_font = nil

--- Returns x, y, w, h, has_icons for the dropdown rectangle.
function M.get_suggestions_rect(ctx, av)
  local suggestions     = ctx.suggestions
  local suggestions_idx = ctx.suggestions_idx
  local partial         = ctx.partial
  local icons           = ctx.icons

  if #suggestions == 0 then
    last_max_width = 0
    return 0, 0, 0, 0
  end

  local line, col = av.doc:get_selection()
  local x, y = av:get_line_screen_position(line, col - #partial)
  y = y + av:get_line_height() + style.padding.y
  local font       = av:get_font()
  local th         = font:get_height()
  local has_icons  = false
  local hide_info  = config.plugins.autocomplete.hide_info
  local hide_icons = config.plugins.autocomplete.hide_icons
  local ah         = config.plugins.autocomplete.max_height
  local show_count = math.min(#suggestions, ah)
  local start_idx  = math.max(suggestions_idx - (ah - 1), 1)

  local max_width, max_l_icon_width = 0, 0
  for i = start_idx, start_idx + show_count - 1 do
    local s = suggestions[i]
    local w = font:get_width(s.text)
    if s.info and not hide_info then
      w = w + style.font:get_width(s.info) + style.padding.x
    end
    local icon = s.icon or s.info
    if not hide_icons and icon and icons[icon] then
      local iw = icons[icon].font:get_width(icons[icon].char)
      if config.plugins.autocomplete.icon_position == "left" then
        max_l_icon_width = math.max(max_l_icon_width, iw + style.padding.x / 2)
      end
      w = w + iw + style.padding.x / 2
      has_icons = true
    end
    max_width = math.max(max_width, w)
  end
  max_width      = math.max(last_max_width, max_width)
  last_max_width = max_width
  max_width      = max_width + style.padding.x * 2
  x              = x - style.padding.x - max_l_icon_width

  -- +1 row for the item count footer
  local max_items = math.min(ah, #suggestions) + 1

  if max_width > core.root_view.size.x then max_width = core.root_view.size.x end
  if max_width < 150 * SCALE then max_width = 150 * SCALE end
  if x + max_width > core.root_view.size.x then
    x = (av.size.x + av.position.x) - max_width
  end

  return x, y - style.padding.y, max_width,
    max_items * (th + style.padding.y) + style.padding.y, has_icons
end

-- Wraps a single line to at most max_chars wide, inserting hyphens at word
-- boundaries. Returns a string when no wrapping is needed, a table otherwise.
local function wrap_line(line, max_chars)
  if #line <= max_chars then return line end
  local lines    = {}
  local line_len = #line
  local new_line = ""
  local prev_char = ""
  local position = 0
  local indent   = line:match("^%s+")
  for char in line:gmatch(".") do
    position = position + 1
    if #new_line < max_chars then
      new_line  = new_line .. char
      prev_char = char
      if position >= line_len then table.insert(lines, new_line) end
    else
      if not prev_char:match("%s")
        and not string.sub(line, position + 1, 1):match("%s")
        and position < line_len
      then
        new_line = new_line .. "-"
      end
      table.insert(lines, new_line)
      new_line = indent and (indent .. char) or char
    end
  end
  return lines
end

local function draw_description_box(text, av, sx, sy, sw)
  if not desc_font or previous_scale ~= SCALE then
    desc_font      = style.code_font:copy(config.plugins.autocomplete.desc_font_size * SCALE)
    previous_scale = SCALE
  end

  local font       = desc_font
  local lh         = font:get_height()
  local y          = sy + style.padding.y
  local x          = sx + sw + style.padding.x / 4
  local width      = 0
  local char_width = font:get_width(" ")
  local draw_left  = false
  local max_chars

  if sx - av.position.x < av.size.x - (sx - av.position.x) - sw then
    max_chars = (av.size.x + av.position.x - x) / char_width - 5
  else
    draw_left = true
    max_chars = (sx - av.position.x - style.padding.x / 4 - style.scrollbar_size)
      / char_width - 5
  end

  local lines = {}
  for line in string.gmatch(text .. "\n", "(.-)\n") do
    local wrapped = wrap_line(line, max_chars)
    if type(wrapped) == "table" then
      for _, wl in pairs(wrapped) do
        width = math.max(width, font:get_width(wl))
        table.insert(lines, wl)
      end
    else
      width = math.max(width, font:get_width(line))
      table.insert(lines, line)
    end
  end

  if draw_left then
    x = sx - style.padding.x / 4 - width - style.padding.x * 2
  end

  local height = #lines * font:get_height()
  renderer.draw_rect(x, sy, width + style.padding.x * 2, height + style.padding.y * 2, style.background3)
  for _, line in pairs(lines) do
    common.draw_text(font, style.text, line, "left", x + style.padding.x, y, width, lh)
    y = y + lh
  end
end

--- Draw the autocomplete dropdown. ctx must carry: suggestions, suggestions_idx,
--- suggestions_offset, partial, icons.
function M.draw_suggestions_box(ctx, av)
  local suggestions        = ctx.suggestions
  local suggestions_idx    = ctx.suggestions_idx
  local suggestions_offset = ctx.suggestions_offset
  local icons              = ctx.icons

  if #suggestions <= 0 then return end

  local ah = config.plugins.autocomplete.max_height
  local rx, ry, rw, rh, has_icons = M.get_suggestions_rect(ctx, av)
  renderer.draw_rect(rx, ry, rw, rh, style.background3)

  local font       = av:get_font()
  local lh         = font:get_height() + style.padding.y
  local y          = ry + style.padding.y / 2
  local show_count = math.min(#suggestions, ah)
  local hide_info  = config.plugins.autocomplete.hide_info

  for i = suggestions_offset, suggestions_offset + show_count - 1 do
    if not suggestions[i] then break end
    local s = suggestions[i]
    local icon_l_padding, icon_r_padding = 0, 0

    if has_icons then
      local icon = s.icon or s.info
      if icon and icons[icon] then
        local ifont  = icons[icon].font
        local itext  = icons[icon].char
        local icolor = icons[icon].color
        if i == suggestions_idx then
          icolor = style.accent
        elseif type(icolor) == "string" then
          icolor = style.syntax[icolor]
        end
        if config.plugins.autocomplete.icon_position == "left" then
          common.draw_text(ifont, icolor, itext, "left", rx + style.padding.x, y, rw, lh)
          icon_l_padding = ifont:get_width(itext) + style.padding.x / 2
        else
          common.draw_text(ifont, icolor, itext, "right", rx, y, rw - style.padding.x, lh)
          icon_r_padding = ifont:get_width(itext) + style.padding.x / 2
        end
      end
    end

    local info_size = 0
    if s.info and not hide_info then
      info_size = style.font:get_width(s.info) + style.padding.x
    end
    local color     = (i == suggestions_idx) and style.accent or style.text

    core.push_clip_rect(
      rx + icon_l_padding + style.padding.x, y,
      rw - info_size - icon_l_padding - icon_r_padding - style.padding.x, lh
    )
    local x_adv = common.draw_text(
      font, color, s.text, "left",
      rx + icon_l_padding + style.padding.x, y, rw, lh
    )
    core.pop_clip_rect()

    if x_adv > rx + rw - info_size - icon_r_padding then
      local ellipsis_size = font:get_width("…")
      local ell_x = rx + rw - info_size - icon_r_padding - ellipsis_size
      renderer.draw_rect(ell_x, y, ellipsis_size, lh, style.background3)
      common.draw_text(font, color, "…", "left", ell_x, y, ellipsis_size, lh)
    end

    if s.info and not hide_info then
      color = (i == suggestions_idx) and style.text or style.dim
      common.draw_text(
        style.font, color, s.info, "right",
        rx, y, rw - icon_r_padding - style.padding.x, lh
      )
    end

    y = y + lh

    if suggestions_idx == i then
      if s.onhover then s.onhover(suggestions_idx, s); s.onhover = nil end
      if s.desc and #s.desc > 0 then
        draw_description_box(s.desc, av, rx, ry, rw)
      end
    end
  end

  renderer.draw_rect(rx, y, rw, 2, style.caret)
  renderer.draw_rect(rx, y + 2, rw, lh, style.background)
  common.draw_text(style.font, style.accent, "Items", "left", rx + style.padding.x, y, rw, lh)
  common.draw_text(
    style.font, style.accent,
    tostring(suggestions_idx) .. "/" .. tostring(#suggestions),
    "right", rx, y, rw - style.padding.x, lh
  )
end

return M
