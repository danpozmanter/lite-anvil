-- mod-version:4
local common = require "core.common"
local config = require "core.config"
local style = require "core.style"
local DocView = require "core.docview"
local native_affordance = require "affordance_model"

config.plugins.bracketmatch = common.merge({
  highlight_color = nil,
}, config.plugins.bracketmatch)

local function update_cache(dv)
  local doc = dv.doc
  local line1, col1, line2, col2 = doc:get_selection()
  if line1 ~= line2 or col1 ~= col2 then
    dv._bm_pos = nil
    dv._bm_key = nil
    return
  end
  local change_id = doc:get_change_id()
  local key = line1 .. "," .. col1 .. "," .. change_id
  if dv._bm_key == key then
    return
  end
  dv._bm_key = key
  local pair = native_affordance.bracket_pair(doc.lines, line1, col1)
  if not pair and col1 > 1 then
    pair = native_affordance.bracket_pair(doc.lines, line1, col1 - 1)
  end
  dv._bm_pos = pair
end

local old_update = DocView.update
function DocView:update(...)
  old_update(self, ...)
  if self:is(DocView) then
    update_cache(self)
  end
end

local old_draw_line_body = DocView.draw_line_body
function DocView:draw_line_body(line, x, y)
  local result = old_draw_line_body(self, line, x, y)
  if self._bm_pos then
    local p = self._bm_pos
    local color = config.plugins.bracketmatch.highlight_color or style.caret
    local lh = self:get_line_height()
    local uw = math.max(2, math.floor(2 * SCALE))
    for i = 1, 3, 2 do
      if p[i] == line then
        local bc = p[i + 1]
        local x1 = x + self:get_col_x_offset(line, bc)
        local x2 = x + self:get_col_x_offset(line, bc + 1)
        renderer.draw_rect(x1, y + lh - uw, x2 - x1, uw, color)
      end
    end
  end
  return result
end
