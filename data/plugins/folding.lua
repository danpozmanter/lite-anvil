-- mod-version:4
local core = require "core"
local common = require "core.common"
local command = require "core.command"
local config = require "core.config"
local keymap = require "core.keymap"
local storage = require "core.storage"
local Doc = require "core.doc"
local DocView = require "core.docview"
local style = require "core.style"
local native_affordance = require "affordance_model"

config.plugins.folding = common.merge({
  persist = true,
}, config.plugins.folding)

local STORAGE_MODULE = "folding"

local function doc_folds(doc)
  doc.folds = doc.folds or {}
  return doc.folds
end

local function get_fold_end(doc, line)
  return native_affordance.get_fold_end(doc.lines, line)
end

local function visible_line_count(doc)
  return native_affordance.visible_line_count(#doc.lines, doc_folds(doc))
end

local function actual_to_visible(doc, line)
  return native_affordance.actual_to_visible(line, doc_folds(doc))
end

local function visible_to_actual(doc, visible)
  return native_affordance.visible_to_actual(visible, #doc.lines, doc_folds(doc))
end

local function next_visible_line(doc, line)
  return native_affordance.next_visible_line(line, doc_folds(doc))
end

local function toggle_fold(doc, line)
  local folds = doc_folds(doc)
  if folds[line] then
    folds[line] = nil
    return
  end
  local end_line = get_fold_end(doc, line)
  if end_line then
    folds[line] = end_line
  end
end

local function save_doc_folds(doc)
  if not config.plugins.folding.persist or not doc.abs_filename then
    return
  end
  local folded = {}
  for line in pairs(doc_folds(doc)) do
    folded[#folded + 1] = line
  end
  table.sort(folded)
  storage.save(STORAGE_MODULE, doc.abs_filename, folded)
end

local function load_doc_folds(doc)
  if not config.plugins.folding.persist or not doc.abs_filename then
    return
  end
  doc.folds = {}
  for _, line in ipairs(storage.load(STORAGE_MODULE, doc.abs_filename) or {}) do
    local end_line = get_fold_end(doc, line)
    if end_line then
      doc.folds[line] = end_line
    end
  end
end

local old_open_doc = core.open_doc
function core.open_doc(filename, ...)
  local doc = old_open_doc(filename, ...)
  if doc then
    load_doc_folds(doc)
  end
  return doc
end

local old_doc_close = Doc.on_close
function Doc:on_close()
  save_doc_folds(self)
  old_doc_close(self)
end

local old_doc_change = Doc.on_text_change
function Doc:on_text_change(change_type)
  if change_type ~= "selection" then
    self.folds = {}
  end
  old_doc_change(self, change_type)
end

local old_get_scrollable_size = DocView.get_scrollable_size
function DocView:get_scrollable_size()
  if not self.doc.folds or not next(self.doc.folds) then
    return old_get_scrollable_size(self)
  end
  local _, _, _, h_scroll = self.h_scrollbar:get_track_rect()
  if not config.scroll_past_end then
    return self:get_line_height() * visible_line_count(self.doc) + style.padding.y * 2 + h_scroll
  end
  return self:get_line_height() * math.max(0, visible_line_count(self.doc) - 1) + self.size.y
end

local old_get_line_screen_position = DocView.get_line_screen_position
function DocView:get_line_screen_position(line, col)
  if not self.doc.folds or not next(self.doc.folds) then
    return old_get_line_screen_position(self, line, col)
  end
  local x, y = self:get_content_offset()
  local lh = self:get_line_height()
  local gw = self:get_gutter_width()
  y = y + (actual_to_visible(self.doc, line) - 1) * lh + style.padding.y
  if col then
    return x + gw + self:get_col_x_offset(line, col), y
  end
  return x + gw, y
end

local old_get_visible_line_range = DocView.get_visible_line_range
function DocView:get_visible_line_range()
  if not self.doc.folds or not next(self.doc.folds) then
    return old_get_visible_line_range(self)
  end
  local _, y, _, y2 = self:get_content_bounds()
  local lh = self:get_line_height()
  local min_visible = math.max(1, math.floor((y - style.padding.y) / lh) + 1)
  local max_visible = math.min(visible_line_count(self.doc), math.floor((y2 - style.padding.y) / lh) + 1)
  return visible_to_actual(self.doc, min_visible), visible_to_actual(self.doc, max_visible)
end

local old_resolve_screen_position = DocView.resolve_screen_position
function DocView:resolve_screen_position(x, y)
  if not self.doc.folds or not next(self.doc.folds) then
    return old_resolve_screen_position(self, x, y)
  end
  local ox, oy = self:get_line_screen_position(1)
  local visible = math.floor((y - oy) / self:get_line_height()) + 1
  local line = visible_to_actual(self.doc, common.clamp(visible, 1, visible_line_count(self.doc)))
  local col = self:get_x_offset_col(line, x - ox)
  return line, col
end

local old_draw = DocView.draw
function DocView:draw()
  if not self.doc.folds or not next(self.doc.folds) then
    return old_draw(self)
  end
  self:draw_background(style.background)
  local _, indent_size = self.doc:get_indent_info()
  self:get_font():set_tab_size(indent_size)
  local minline, maxline = self:get_visible_line_range()
  local lh = self:get_line_height()
  local gw, gpad = self:get_gutter_width()
  local x, y = self:get_line_screen_position(minline)
  local line = minline
  while line <= maxline do
    y = y + (self:draw_line_gutter(line, self.position.x, y, gpad and gw - gpad or gw) or lh)
    line = next_visible_line(self.doc, line)
  end
  local pos = self.position
  x, y = self:get_line_screen_position(minline)
  core.push_clip_rect(pos.x + gw, pos.y, self.size.x - gw, self.size.y)
  line = minline
  while line <= maxline do
    y = y + (self:draw_line_body(line, x, y) or lh)
    line = next_visible_line(self.doc, line)
  end
  self:draw_overlay()
  core.pop_clip_rect()
  self:draw_scrollbar()
end

local old_draw_line_gutter = DocView.draw_line_gutter
function DocView:draw_line_gutter(line, x, y, width)
  local lh = old_draw_line_gutter(self, line, x, y, width)
  local end_line = get_fold_end(self.doc, line)
  if end_line then
    local icon = self.doc.folds and self.doc.folds[line] and ">" or "v"
    common.draw_text(style.icon_font, style.dim, icon, nil, x + 2, y, 10, lh)
  end
  return lh
end

local old_draw_line_text = DocView.draw_line_text
function DocView:draw_line_text(line, x, y)
  local lh = old_draw_line_text(self, line, x, y)
  local end_line = self.doc.folds and self.doc.folds[line]
  if end_line then
    local text = string.format(" ... %d lines", end_line - line)
    renderer.draw_text(self:get_font(), text, x + self:get_col_x_offset(line, math.huge) + style.padding.x, y + self:get_line_text_y_offset(), style.dim)
  end
  return lh
end

local old_mouse_pressed = DocView.on_mouse_pressed
function DocView:on_mouse_pressed(button, x, y, clicks)
  if button == "left" and self.hovering_gutter then
    local line = self:resolve_screen_position(x, y)
    if x <= self.position.x + 12 and get_fold_end(self.doc, line) then
      toggle_fold(self.doc, line)
      save_doc_folds(self.doc)
      core.redraw = true
      return true
    end
  end
  return old_mouse_pressed(self, button, x, y, clicks)
end

local old_previous_line = DocView.translate.previous_line
DocView.translate.previous_line = function(doc, line, col, dv)
  local visible = actual_to_visible(doc, line)
  if visible <= 1 then
    return 1, 1
  end
  local target = visible_to_actual(doc, visible - 1)
  return target, dv:get_x_offset_col(target, dv.last_x_offset.offset or 0)
end

local old_next_line = DocView.translate.next_line
DocView.translate.next_line = function(doc, line, col, dv)
  local visible = actual_to_visible(doc, line)
  if visible >= visible_line_count(doc) then
    return #doc.lines, math.huge
  end
  local target = visible_to_actual(doc, visible + 1)
  return target, dv:get_x_offset_col(target, dv.last_x_offset.offset or 0)
end

command.add("core.docview", {
  ["fold:toggle"] = function(dv)
    local line = select(1, dv.doc:get_selection())
    toggle_fold(dv.doc, line)
    save_doc_folds(dv.doc)
  end,
})

keymap.add {
  ["ctrl+alt+["] = "fold:toggle",
}
