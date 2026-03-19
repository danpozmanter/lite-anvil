use mlua::prelude::*;

/// Thin Lua bootstrap: TreeView:extend OOP + drawing methods + command/keymap
/// registration. All state init and cache management delegate to Rust native.
const BOOTSTRAP: &str = r#"-- mod-version:4
local core = require "core"
local common = require "core.common"
local command = require "core.command"
local config = require "core.config"
local keymap = require "core.keymap"
local style = require "core.style"
local View = require "core.view"
local ContextMenu = require "core.contextmenu"
local RootView = require "core.rootview"
local CommandView = require "core.commandview"
local DocView = require "core.docview"
local native_tree_model = require "tree_model"
local native = require "treeview_native"

config.plugins.treeview = common.merge({
  size = 200 * SCALE,
  highlight_focused_file = true,
  expand_dirs_to_focused_file = false,
  scroll_to_focused_file = false,
  animate_scroll_to_focused_file = true,
  show_hidden = false,
  show_ignored = true,
  visible = true,
  max_dir_entries = 5000,
}, config.plugins.treeview)

local tooltip_offset = style.font:get_height()
local tooltip_border = 1
local tooltip_delay = 0.5
local tooltip_alpha = 255
local tooltip_alpha_rate = 1
local icon_vertical_nudge = common.round(1 * SCALE)
local separator_inset = 10
local LABEL_CACHE_MAX = 3000


local function replace_alpha(color, alpha)
  local r, g, b = table.unpack(color)
  return { r, g, b, alpha }
end


local TreeView = View:extend()

function TreeView:__tostring() return "TreeView" end

function TreeView:new()
  TreeView.super.new(self)
  native.init(self)
end


function TreeView:set_target_size(axis, value)
  if axis == "x" then
    self.target_size = math.max(140 * SCALE, common.round(value))
    config.plugins.treeview.size = self.target_size
    if core.session then
      core.session.treeview_size = self.target_size
    end
    return true
  end
end

function TreeView:get_name()
  return nil
end


function TreeView:get_item_height()
  return style.font:get_height() + style.padding.y
end

function TreeView:each_item()
  return coroutine.wrap(function()
    local ox, oy = self:get_content_offset()
    local h = self:get_item_height()
    self:sync_model()
    self.count_lines = self.visible_count
    for i = 1, self.visible_count do
      local item = self:get_item_by_row(i)
      coroutine.yield(item, ox, oy + style.padding.y + h * (i - 1), self.size.x, h)
    end
  end)
end


function TreeView:sync_model()
  if not self.items_dirty and not (#core.projects ~= self.last_project_count) then
    return
  end
  native.sync_model(self)
end


function TreeView:get_item_by_row(row)
  return native.get_item_by_row(self, row)
end

function TreeView:get_items_in_range(start_row, end_row)
  return native.get_items_in_range(self, start_row, end_row)
end


function TreeView:resolve_path(path)
  if not path then return nil end
  self:sync_model()
  local idx = native_tree_model.get_row(self.model_roots, path)
  if idx then
    local ox, oy = self:get_content_offset()
    local h = self:get_item_height()
    local y = oy + style.padding.y + h * (idx - 1)
    return self:get_item_by_row(idx), ox, y, self.size.x, h
  end
end


function TreeView:get_selected_item()
  if self.selected_path then
    local item = self:resolve_path(self.selected_path)
    if item then
      self.selected_item = item
      return item
    end
    self.selected_item = nil
    self.selected_path = nil
  end
  return self.selected_item
end


function TreeView:get_hovered_item()
  if self.hovered_path then
    local item = self:resolve_path(self.hovered_path)
    if item then
      self.hovered_item = item
      return item
    end
    self.hovered_item = nil
    self.hovered_path = nil
  end
  return self.hovered_item
end


function TreeView:set_selection(selection, selection_y, center, instant)
  -- item_height is passed so Rust can compute scroll without calling font userdata methods.
  local h = (selection and selection_y) and self:get_item_height() or 0
  native.set_selection(self, selection, selection_y, center, instant, h)
end

function TreeView:set_selection_to_path(path, expand, scroll_to, instant)
  if expand then
    native_tree_model.expand_to(path)
    self.items_dirty = true
  end
  self:sync_model()
  local to_select, _, to_select_y = self:resolve_path(path)
  if to_select then
    self:set_selection(to_select, scroll_to and to_select_y, true, instant)
  end
  return to_select
end


function TreeView:get_text_bounding_box(item, x, y, w, h)
  local xoffset = item.depth * style.padding.x
    + style.padding.x
    + self.item_chevron_width
    + self.item_icon_width
    + self.item_text_spacing
  x = x + xoffset
  local cached_width = self:get_label_cache(item.abs_filename, item.name, self.size.x).width_cache
  w = cached_width.width + 2 * style.padding.x
  return x, y, w, h
end

function TreeView:get_label_cache(path, text, avail_width)
  local width_key = math.max(0, math.floor(avail_width or 0))
  local cached = self.label_cache[path]
  if cached and cached.text == text and cached.width_key == width_key then
    return cached
  end
  local full_width = style.font:get_width(text)
  local display_text = text
  if full_width > width_key and width_key > 0 then
    local dots = "…"
    local low, high = 0, #text
    while low < high do
      local mid = math.floor((low + high + 1) / 2)
      local candidate = text:sub(1, mid) .. dots
      if style.font:get_width(candidate) <= math.max(0, width_key - style.padding.x) then
        low = mid
      else
        high = mid - 1
      end
    end
    display_text = low > 0 and (text:sub(1, low) .. dots) or dots
  end
  cached = {
    text = text,
    width_key = width_key,
    display_text = display_text,
    width_cache = {
      name = text,
      width = full_width,
    },
  }
  if self.label_cache_count >= LABEL_CACHE_MAX then
    self.label_cache = {}
    self.text_width_cache = {}
    self.label_cache_count = 0
  end
  self.label_cache[path] = cached
  self.text_width_cache[path] = cached.width_cache
  self.label_cache_count = self.label_cache_count + 1
  return cached
end


function TreeView:on_mouse_moved(px, py, ...)
  if not self.visible then return end
  if TreeView.super.on_mouse_moved(self, px, py, ...) then
    self.hovered_item = nil
    self.hovered_path = nil
    return
  end
  self:sync_model()
  local ox, oy = self:get_content_offset()
  local h = self:get_item_height()
  local pad = style.padding.y
  local row = math.floor((py - oy - pad) / h)
  local item = row >= 0 and self:get_item_by_row(row + 1) or nil
  local in_text_box, same_hover = false, false
  if item and px > ox and px <= ox + self.size.x then
    same_hover = self.hovered_path == item.abs_filename
    local ix, iy, iw, ih = self:get_text_bounding_box(item, ox, oy + pad + row * h, self.size.x, h)
    in_text_box = px > ix and py > iy and px <= ix + iw and py <= iy + ih
  else
    item = nil
  end
  native.update_hover(self, item, in_text_box, px, py, same_hover, system.get_time())
end


function TreeView:on_mouse_left()
  TreeView.super.on_mouse_left(self)
  self.hovered_item = nil
  self.hovered_path = nil
end


function TreeView:update()
  local dest = self.visible and self.target_size or 0
  if self.init_size then
    self.size.x = dest
    self.init_size = false
  else
    self:move_towards(self.size, "x", dest, 0.35, "treeview")
  end

  if self.size.x == 0 or self.size.y == 0 or not self.visible then return end

  local duration = system.get_time() - self.tooltip.begin
  if self.hovered_path and self.tooltip.x and duration > tooltip_delay then
    self:move_towards(self.tooltip, "alpha", tooltip_alpha, tooltip_alpha_rate, "treeview")
  else
    self.tooltip.alpha = 0
  end

  local dy = math.abs(self.last_scroll_y - self.scroll.y)
  if dy > 0 then
    self:on_mouse_moved(core.root_view.mouse.x, core.root_view.mouse.y, 0, 0)
    self.last_scroll_y = self.scroll.y
  end

  local cfg = config.plugins.treeview
  if native_tree_model.generation() ~= self.last_tree_generation then
    self.items_dirty = true
  end
  if cfg.highlight_focused_file then
    local current_node = core.root_view:get_active_node()
    local current_active_view = core.active_view
    if current_node and not current_node.locked
     and current_active_view ~= self and current_active_view ~= self.last_active_view then
      self.last_active_view = current_active_view
      if DocView:is_extended_by(current_active_view) then
        local abs_filename = current_active_view.doc
                             and current_active_view.doc.abs_filename or ""
        self:set_selection_to_path(abs_filename,
                                   cfg.expand_dirs_to_focused_file,
                                   cfg.scroll_to_focused_file,
                                   not cfg.animate_scroll_to_focused_file)
      else
        self:set_selection(nil)
      end
    end
  end

  TreeView.super.update(self)
end

function TreeView:on_scale_change()
  local icon_w = math.max(style.icon_font:get_width("D"), style.icon_font:get_width("d"), style.icon_font:get_width("f"))
  local chev_w = math.max(style.icon_font:get_width("+"), style.icon_font:get_width("-"), style.padding.x)
  native.apply_scale_metrics(self, icon_w, chev_w,
    math.max(style.padding.x, math.ceil(icon_w * 0.4)),
    style.font:get_height(), style.icon_font:get_height())
end


function TreeView:get_scrollable_size()
  return self.count_lines and self:get_item_height() * (self.count_lines + 1) or math.huge
end


function TreeView:draw_tooltip()
  local hovered = self:get_hovered_item()
  if not hovered then return end
  local text = common.home_encode(hovered.abs_filename)
  local w, h = style.font:get_width(text), style.font:get_height(text)

  local _, _, row_y = self:resolve_path(hovered.abs_filename)
  row_y = row_y or self.tooltip.y
  local row_h = self:get_item_height()
  local x, y = self.tooltip.x + tooltip_offset, self.tooltip.y + tooltip_offset
  w, h = w + style.padding.x, h + style.padding.y

  if x + w > core.root_view.root_node.size.x - style.padding.x then
    x = self.tooltip.x - w - tooltip_offset
  end
  if x < style.padding.x then
    x = style.padding.x
  end
  if y >= row_y and y <= row_y + row_h then
    y = row_y - h - tooltip_offset
  end
  if y < style.padding.x then
    y = math.min(core.root_view.root_node.size.y - h - style.padding.y, row_y + row_h + tooltip_offset)
  end

  local bx, by = x - tooltip_border, y - tooltip_border
  local bw, bh = w + 2 * tooltip_border, h + 2 * tooltip_border
  renderer.draw_rect(bx, by, bw, bh, replace_alpha(style.text, self.tooltip.alpha))
  renderer.draw_rect(x, y, w, h, replace_alpha(style.background2, self.tooltip.alpha))
  common.draw_text(style.font, replace_alpha(style.text, self.tooltip.alpha), text, "center", x, y, w, h)
end


function TreeView:get_item_icon(item, active, hovered)
  local character = "f"
  if item.type == "dir" then
    character = item.expanded and "D" or "d"
  end
  local font = style.icon_font
  local color = style.text
  if active or hovered then
    color = style.accent
  elseif item.ignored then
    color = style.dim
  end
  return character, font, color
end

function TreeView:get_item_text(item, active, hovered)
  local available_width = self.size.x
    - (item.depth * style.padding.x + style.padding.x * 2 + self.item_chevron_width + self.item_icon_width + self.item_text_spacing)
  local text = self:get_label_cache(item.abs_filename, item.name, available_width).display_text
  local font = style.font
  local color = style.text
  if active or hovered then
    color = style.accent
  elseif item.ignored then
    color = style.dim
  end
  return text, font, color
end


function TreeView:draw_item_text(item, active, hovered, x, y, w, h)
  local item_text, item_font, item_color = self:get_item_text(item, active, hovered)
  common.draw_text(item_font, item_color, item_text, nil, x, y, 0, h)
end


function TreeView:draw_item_icon(item, active, hovered, x, y, w, h)
  local icon_char, icon_font, icon_color = self:get_item_icon(item, active, hovered)
  local text_top = y + common.round((h - style.font:get_height()) / 2)
  local iy = text_top + common.round((style.font:get_height() - self.icon_font_height) / 2) - icon_vertical_nudge
  renderer.draw_text(icon_font, icon_char, x, iy, icon_color)
  return self.item_icon_width + self.item_text_spacing
end


function TreeView:draw_item_body(item, active, hovered, x, y, w, h)
  x = x + self:draw_item_icon(item, active, hovered, x, y, w, h)
  self:draw_item_text(item, active, hovered, x, y, w, h)
end


function TreeView:draw_item_chevron(item, active, hovered, x, y, w, h)
  if item.type == "dir" then
    local chevron_icon = item.expanded and "-" or "+"
    local chevron_color = hovered and style.accent or style.text
    local text_top = y + common.round((h - style.font:get_height()) / 2)
    local iy = text_top + common.round((style.font:get_height() - self.icon_font_height) / 2) - icon_vertical_nudge
    renderer.draw_text(style.icon_font, chevron_icon, x, iy, chevron_color)
  end
  return self.item_chevron_width
end


function TreeView:draw_item_background(item, active, hovered, x, y, w, h)
  if active then
    local active_color = { table.unpack(style.line_highlight) }
    active_color[4] = math.max(active_color[4] or 0, 210)
    renderer.draw_rect(x, y, w, h, active_color)
  end
  if hovered and not active then
    local hover_color = { table.unpack(style.line_highlight) }
    hover_color[4] = 110
    renderer.draw_rect(x, y, w, h, hover_color)
  end
end


function TreeView:draw_item(item, active, hovered, x, y, w, h)
  self:draw_item_background(item, active, hovered, x, y, w, h)

  x = x + item.depth * style.padding.x + style.padding.x
  x = x + self:draw_item_chevron(item, active, hovered, x, y, w, h)

  self:draw_item_body(item, active, hovered, x, y, w, h)
end


function TreeView:draw()
  if not self.visible then return end
  self:draw_background(style.background2)
  renderer.draw_rect(
    self.position.x + separator_inset,
    self.position.y,
    math.max(0, self.size.x - separator_inset * 2),
    style.divider_size,
    style.divider
  )

  if #core.projects ~= self.last_project_count then
    self.items_dirty = true
  end
  self:sync_model()

  local _y, _h = self.position.y, self.size.y
  local ox, oy = self:get_content_offset()
  local h = self:get_item_height()
  local pad = style.padding.y
  local first_row = math.max(1, math.floor((_y - oy - pad) / h) + 1)
  local last_row = math.min(self.visible_count, math.floor((_y + _h - oy - pad) / h) + 1)
  local items = self:get_items_in_range(first_row, last_row)

  for offset, item in ipairs(items) do
    if item then
      local row = first_row + offset - 1
      local y = oy + pad + (row - 1) * h
      self:draw_item(item,
        item.abs_filename == self.selected_path,
        item.abs_filename == self.hovered_path,
        ox, y, self.size.x, h)
    end
  end

  self:draw_scrollbar()
  if self.hovered_path and self.tooltip.x and self.tooltip.alpha > 0 then
    core.root_view:defer_draw(self.draw_tooltip, self)
  end
end


function TreeView:get_parent(item)
  item = item or self:get_selected_item()
  if not item then return end
  local parent_path = common.dirname(item.abs_filename)
  if not parent_path then return end
  local it, _, y = self:resolve_path(parent_path)
  if it then
    return it, y
  end
end


function TreeView:get_item(item, where)
  self:sync_model()
  local idx = item and native_tree_model.get_row(self.model_roots, item.abs_filename) or nil
  if not idx then
    idx = where >= 0 and 1 or self.visible_count
  else
    idx = idx + where
  end
  idx = common.clamp(idx, 1, self.visible_count)
  local target = self:get_item_by_row(idx)
  if target then
    return self:resolve_path(target.abs_filename)
  end
end

function TreeView:get_next(item)
  return self:get_item(item, 1)
end

function TreeView:get_previous(item)
  return self:get_item(item, -1)
end


function TreeView:toggle_expand(toggle, item)
  item = item or self:get_selected_item()
  if not item then return end
  if item.type == "dir" then
    native_tree_model.toggle_expand(
      item.abs_filename,
      type(toggle) == "boolean" and toggle or nil
    )
    self.items_dirty = true
  end
end

function TreeView:open_doc(filename)
  core.root_view:open_doc(core.open_doc(filename))
end

local view

local function invalidate_project_tree(project)
  if project and project.path then
    native_tree_model.invalidate(project.path)
  end
  view.items_dirty = true
end

-- init
view = TreeView()
view:on_scale_change()
local node = core.root_view:get_active_node()
view.node = node:split("left", view, {x = true}, true)

local toolbar_view = nil
local toolbar_plugin, ToolbarView = pcall(require, "plugins.toolbarview")
if config.plugins.toolbarview ~= false and toolbar_plugin then
  toolbar_view = ToolbarView()
  view.node:split("down", toolbar_view, {y = true})
  local min_toolbar_width = toolbar_view:get_min_width()
  view:set_target_size("x", math.max(config.plugins.treeview.size, min_toolbar_width))
  command.add(nil, {
    ["toolbar:toggle"] = function()
      toolbar_view:toggle_visible()
    end,
  })
end


local old_remove_project = core.remove_project
function core.remove_project(project, force)
  local project = old_remove_project(project, force)
  view.items_dirty = true
  native.sync_model(view)
end

local on_quit_project = core.on_quit_project
function core.on_quit_project()
  view.items_dirty = true
  native_tree_model.clear_all()
  on_quit_project()
end

local function is_project_folder(item)
  if not item or not item.project then
    return false
  end
  return item.abs_filename == item.project.path
end

local function is_primary_project_folder(path)
  return core.root_project().path == path
end


local function treeitem()
  return view:get_hovered_item() or view:get_selected_item()
end

function TreeView:on_context_menu()
  return { items = {
    { text = "Open in System", command = "treeview:open-in-system" },
    ContextMenu.DIVIDER,
    { text = "Rename", command = "treeview:rename" },
    { text = "Delete", command = "treeview:delete" },
    { text = "New File", command = "treeview:new-file" },
    { text = "New Folder", command = "treeview:new-folder" },
    { text = "Remove directory", command = "treeview:remove-project-directory" },
    { text = "Find in Directory", command = "treeview:search-in-directory" }
  } }, self
end

if config.plugins.projectsearch ~= false then
  command.add(function(active_view)
    local hovered = view:get_hovered_item()
    return hovered and hovered.type == "dir"
      and (active_view or core.active_view) == view
  end, {
    ["treeview:search-in-directory"] = function(item)
      local hovered = view:get_hovered_item()
      if hovered then
        command.perform("project-search:find", hovered.abs_filename)
      end
    end
  })
end

command.add(function(active_view)
  local item = treeitem()
  return item ~= nil
    and not is_project_folder(item)
    and (active_view or core.active_view) == view, item
end, {
  ["treeview:delete"] = function(item)
    local filename = item.abs_filename
    local relfilename = item.filename
    if item.project ~= core.root_project() then
      relfilename = common.basename(item.abs_filename) .. PATHSEP .. relfilename
    end
    local file_info = system.get_file_info(filename)
    local file_type = file_info.type == "dir" and "Directory" or "File"
    local opt = {
      { text = "Yes", default_yes = true },
      { text = "No", default_no = true }
    }
    core.nag_view:show(
      string.format("Delete %s", file_type),
      string.format(
        "Are you sure you want to delete the %s?\n%s: %s",
        file_type:lower(), file_type, relfilename
      ),
      opt,
      function(choice)
        if choice.text == "Yes" then
          if file_info.type == "dir" then
            local deleted, error, path = common.rm(filename, true)
            if not deleted then
              core.error("Error: %s - \"%s\" ", error, path)
              return
            end
          else
            local removed, error = os.remove(filename)
            if not removed then
              core.error("Error: %s - \"%s\"", error, filename)
              return
            end
          end
          core.log("Deleted \"%s\"", filename)
          invalidate_project_tree(item.project)
        end
      end
    )
  end,

  ["treeview:rename"] = function(item)
    local old_filename = item.project:normalize_path(item.abs_filename)
    local old_abs_filename = item.abs_filename
    core.command_view:enter("Rename", {
      text = old_filename,
      submit = function(filename)
        local abs_filename = item.project:absolute_path(filename)
        local res, err = os.rename(old_abs_filename, abs_filename)
        if res then
          for _, doc in ipairs(core.docs) do
            if doc.abs_filename and old_abs_filename == doc.abs_filename then
              doc:set_filename(filename, abs_filename)
              doc:reset_syntax()
              break
            end
          end
          core.log("Renamed \"%s\" to \"%s\"", old_filename, filename)
        else
          core.error("Error while renaming \"%s\" to \"%s\": %s", old_abs_filename, abs_filename, err)
        end
        invalidate_project_tree(item.project)
      end,
      suggest = function(text)
        return common.path_suggest(text, item.project and item.project.path)
      end
    })
  end
})

local previous_view = nil

command.add(nil, {
  ["treeview:toggle"] = function()
    view.visible = not view.visible
  end,

  ["treeview:toggle-hidden"] = function()
    view.show_hidden = not view.show_hidden
    view.items_dirty = true
  end,

  ["treeview:toggle-ignored"] = function()
    view.show_ignored = not view.show_ignored
    view.items_dirty = true
  end,

  ["treeview:toggle-focus"] = function()
    if not core.active_view:is(TreeView) then
      if core.active_view:is(CommandView) then
        previous_view = core.last_active_view
      else
        previous_view = core.active_view
      end
      if not previous_view then
        previous_view = core.root_view:get_primary_node().active_view
      end
      core.set_active_view(view)
      if not view:get_selected_item() then
        for it, _, y in view:each_item() do
          view:set_selection(it, y)
          break
        end
      end
    else
      core.set_active_view(
        previous_view or core.root_view:get_primary_node().active_view
      )
    end
  end
})

command.add(TreeView, {
  ["treeview:next"] = function()
    local item, _, item_y = view:get_next(view:get_selected_item())
    view:set_selection(item, item_y)
  end,

  ["treeview:previous"] = function()
    local item, _, item_y = view:get_previous(view:get_selected_item())
    view:set_selection(item, item_y)
  end,

  ["treeview:open"] = function()
    local item = view:get_selected_item()
    if not item then return end
    if item.type == "dir" then
      view:toggle_expand()
    else
      core.try(function()
        if core.last_active_view and core.active_view == view then
          core.set_active_view(core.last_active_view)
        end
        view:open_doc(item.project:normalize_path(item.abs_filename))
      end)
    end
  end,

  ["treeview:deselect"] = function()
    view:set_selection(nil)
  end,

  ["treeview:select"] = function()
    view:set_selection(view:get_hovered_item())
  end,

  ["treeview:select-and-open"] = function()
    local hovered = view:get_hovered_item()
    if hovered then
      view:set_selection(hovered)
      command.perform "treeview:open"
    end
  end,

  ["treeview:collapse"] = function()
    local item = view:get_selected_item()
    if item then
      if item.type == "dir" and item.expanded then
        view:toggle_expand(false)
      else
        local parent_item, y = view:get_parent(item)
        if parent_item then
          view:set_selection(parent_item, y)
        end
      end
    end
  end,

  ["treeview:expand"] = function()
    local item = view:get_selected_item()
    if not item or item.type ~= "dir" then return end

    if item.expanded then
      local next_item, _, next_y = view:get_next(item)
      if next_item.depth > item.depth then
        view:set_selection(next_item, next_y)
      end
    else
      view:toggle_expand(true)
    end
  end,
})


command.add(
  function(active_view)
    local item = treeitem()
    return item ~= nil and (active_view or core.active_view) == view, item
  end, {

  ["treeview:new-file"] = function(item)
    local text
    if not is_project_folder(item) then
      if item.type == "dir" then
        text = item.project:normalize_path(item.abs_filename) .. PATHSEP
      elseif item.type == "file" then
        text = item.project:normalize_path(common.dirname(item.abs_filename)) .. PATHSEP
      end
    end
    core.command_view:enter("Filename", {
      text = text,
      submit = function(filename)
        local doc_filename = item.project:absolute_path(filename)
        local file, err = io.open(doc_filename, "a+")
        if not file then
          core.error("Error: unable to create a new file in \"%s\": %s", doc_filename, err)
          return
        end
        file:close()
        view:open_doc(doc_filename)
        core.log("Created %s", doc_filename)
        invalidate_project_tree(item.project)
      end,
      suggest = function(text)
        return common.path_suggest(text, item.project and item.project.path)
      end
    })
  end,

  ["treeview:new-folder"] = function(item)
    local text
    if not is_project_folder(item) then
      if item.type == "dir" then
        text = item.project:normalize_path(item.abs_filename) .. PATHSEP
      elseif item.type == "file" then
        text = item.project:normalize_path(common.dirname(item.abs_filename)) .. PATHSEP
      end
    end
    core.command_view:enter("Folder Name", {
      text = text,
      submit = function(filename)
        local dir_path = item.project:absolute_path(filename)
        local created, err, err_path = common.mkdirp(dir_path)
        if not created then
          core.error("Error: unable to create folder \"%s\": %s (%s)", dir_path, err or "unknown error", err_path or dir_path)
          return
        end
        core.log("Created %s", dir_path)
        invalidate_project_tree(item.project)
      end,
      suggest = function(text)
        return common.path_suggest(text, item.project and item.project.path)
      end
    })
  end,

  ["treeview:open-in-system"] = function(item)
    if PLATFORM == "Windows" then
      system.exec(string.format("start \"\" %q", item.abs_filename))
    elseif string.find(PLATFORM, "Mac") then
      system.exec(string.format("open %q", item.abs_filename))
    elseif PLATFORM == "Linux" or string.find(PLATFORM, "BSD") then
      system.exec(string.format("xdg-open %q", item.abs_filename))
    end
  end
})

command.add(function(active_view)
    local item = treeitem()
    return item
      and not is_primary_project_folder(item.abs_filename)
      and is_project_folder(item)
      and (active_view or core.active_view) == view, item
  end, {
  ["treeview:remove-project-directory"] = function(item)
    core.remove_project(item.project)
  end,
})


keymap.add {
  ["ctrl+\\"]     = "treeview:toggle",
  ["ctrl+h"]      = "treeview:toggle-hidden",
  ["ctrl+i"]      = "treeview:toggle-ignored",
  ["up"]          = "treeview:previous",
  ["down"]        = "treeview:next",
  ["left"]        = "treeview:collapse",
  ["right"]       = "treeview:expand",
  ["return"]      = "treeview:open",
  ["escape"]      = "treeview:deselect",
  ["delete"]      = "treeview:delete",
  ["ctrl+return"] = "treeview:new-folder",
  ["lclick"]      = "treeview:select-and-open",
  ["mclick"]      = "treeview:select",
  ["ctrl+lclick"] = "treeview:new-folder"
}

config.plugins.treeview.config_spec = {
  name = "Treeview",
  {
    label = "Size",
    description = "Default treeview width.",
    path = "size",
    type = "number",
    default = toolbar_view and math.ceil(toolbar_view:get_min_width() / SCALE)
      or 200 * SCALE,
    min = toolbar_view and toolbar_view:get_min_width() / SCALE
      or 200 * SCALE,
    get_value = function(value)
      return value / SCALE
    end,
    set_value = function(value)
      return value * SCALE
    end,
    on_apply = function(value)
      view:set_target_size("x", math.max(
        value, toolbar_view and toolbar_view:get_min_width() or 200 * SCALE
      ))
    end
  },
  {
    label = "Hide on Startup",
    description = "Show or hide the treeview on startup.",
    path = "visible",
    type = "toggle",
    default = false,
    on_apply = function(value)
      view.visible = not value
    end
  }
}

view.toolbar = toolbar_view

return view
"#;

// ── Rust native helpers ──────────────────────────────────────────────────────

fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

/// Initialise all TreeView state fields.
///
/// Reads initial plugin config and restores any session-persisted size, so
/// `TreeView:new()` in Lua only needs to call `super.new` then this function.
fn init(lua: &Lua, self_table: LuaTable) -> LuaResult<()> {
    // `config` is a module-local in the BOOTSTRAP, not a global — require it directly.
    let config: LuaTable = require_table(lua, "core.config")?;
    let plugins: LuaTable = config.get("plugins")?;
    let tv_cfg: LuaTable = plugins.get("treeview")?;
    // Restore session-persisted treeview width if available.
    let core = require_table(lua, "core")?;
    if let LuaValue::Table(session) = core.get::<LuaValue>("session")? {
        let saved: LuaValue = session.get("treeview_size")?;
        match saved {
            LuaValue::Number(_) | LuaValue::Integer(_) => tv_cfg.set("size", saved)?,
            _ => {}
        }
    }
    self_table.set("scrollable", true)?;
    self_table.set("visible", tv_cfg.get::<LuaValue>("visible")?)?;
    self_table.set("init_size", true)?;
    self_table.set("target_size", tv_cfg.get::<LuaValue>("size")?)?;
    self_table.set("show_hidden", tv_cfg.get::<LuaValue>("show_hidden")?)?;
    self_table.set("show_ignored", tv_cfg.get::<LuaValue>("show_ignored")?)?;
    let tooltip = lua.create_table()?;
    tooltip.set("x", LuaValue::Nil)?;
    tooltip.set("y", LuaValue::Nil)?;
    tooltip.set("begin", 0.0f64)?;
    tooltip.set("alpha", 0.0f64)?;
    self_table.set("tooltip", tooltip)?;
    self_table.set("last_scroll_y", 0.0f64)?;
    self_table.set("selected_path", LuaValue::Nil)?;
    self_table.set("hovered_path", LuaValue::Nil)?;
    self_table.set("selected_item", LuaValue::Nil)?;
    self_table.set("hovered_item", LuaValue::Nil)?;
    self_table.set("item_icon_width", 0.0f64)?;
    self_table.set("item_chevron_width", 0.0f64)?;
    self_table.set("item_text_spacing", 0.0f64)?;
    self_table.set("items_dirty", true)?;
    self_table.set("last_project_count", 0i64)?;
    self_table.set("last_tree_generation", 0i64)?;
    self_table.set("visible_count", 0i64)?;
    self_table.set("count_lines", 0i64)?;
    self_table.set("model_roots", lua.create_table()?)?;
    self_table.set("model_opts", LuaValue::Nil)?;
    self_table.set("project_roots", lua.create_table()?)?;
    let range_cache = lua.create_table()?;
    range_cache.set("start_row", 0i64)?;
    range_cache.set("end_row", 0i64)?;
    range_cache.set("items", lua.create_table()?)?;
    self_table.set("range_cache", range_cache)?;
    self_table.set("text_width_cache", lua.create_table()?)?;
    self_table.set("label_cache", lua.create_table()?)?;
    self_table.set("label_cache_count", 0i64)?;
    self_table.set("item_font_height", 0.0f64)?;
    self_table.set("icon_font_height", 0.0f64)?;
    Ok(())
}

/// Build roots/opts/project_roots from live Lua globals, call tree_model.sync_roots,
/// and update all view state fields.
fn sync_model(lua: &Lua, self_table: LuaTable) -> LuaResult<()> {
    let core = require_table(lua, "core")?;
    let projects: LuaTable = core.get("projects")?;
    let pathsep: String = lua.globals().get("PATHSEP").unwrap_or_else(|_| "/".to_string());
    let roots = lua.create_table()?;
    let project_roots = lua.create_table()?;
    let mut project_count = 0i64;
    for entry in projects.sequence_values::<LuaTable>() {
        let project = entry?;
        let path: String = project.get("path")?;
        project_count += 1;
        roots.raw_set(project_count, path.clone())?;
        project_roots.set(path.clone(), project.clone())?;
        if pathsep == "\\" {
            project_roots.set(path.replace('\\', "/"), project)?;
        }
    }
    self_table.set("project_roots", project_roots)?;
    let config: LuaTable = require_table(lua, "core.config")?;
    let show_hidden: bool = self_table.get("show_hidden").unwrap_or(false);
    let show_ignored: bool = self_table.get("show_ignored").unwrap_or(true);
    let plugins: LuaTable = config.get("plugins")?;
    let tv_cfg: LuaTable = plugins.get("treeview")?;
    let max_entries: LuaValue = tv_cfg.get("max_dir_entries")?;
    let file_size_limit: f64 = config.get("file_size_limit").unwrap_or(50.0);
    let ignore_files: LuaValue = config.get("ignore_files")?;
    let (gitignore_enabled, gitignore_additional) = match config.get::<LuaTable>("gitignore") {
        Ok(t) => {
            let enabled: LuaValue = t.get("enabled").unwrap_or(LuaValue::Nil);
            let additional: LuaValue = t.get("additional_patterns").unwrap_or(LuaValue::Nil);
            (!matches!(enabled, LuaValue::Boolean(false)), additional)
        }
        Err(_) => (true, LuaValue::Nil),
    };
    let opts = lua.create_table()?;
    opts.set("show_hidden", show_hidden)?;
    opts.set("show_ignored", show_ignored)?;
    opts.set("max_entries", max_entries)?;
    opts.set("file_size_limit_bytes", file_size_limit * 1_000_000.0)?;
    opts.set("ignore_files", ignore_files)?;
    opts.set("gitignore_enabled", gitignore_enabled)?;
    match gitignore_additional {
        LuaValue::Nil => opts.set("gitignore_additional_patterns", lua.create_table()?)?,
        other => opts.set("gitignore_additional_patterns", other)?,
    }
    let tree_model = require_table(lua, "tree_model")?;
    let sync_roots: LuaFunction = tree_model.get("sync_roots")?;
    sync_roots.call::<()>((roots.clone(), opts))?;
    let generation_fn: LuaFunction = tree_model.get("generation")?;
    let tree_gen: i64 = generation_fn.call(())?;
    let visible_count_fn: LuaFunction = tree_model.get("visible_count")?;
    let vis: i64 = visible_count_fn.call(roots.clone())?;
    self_table.set("last_tree_generation", tree_gen)?;
    self_table.set("visible_count", vis)?;
    self_table.set("count_lines", vis)?;
    self_table.set("items_dirty", false)?;
    self_table.set("last_project_count", project_count)?;
    self_table.set("model_roots", roots)?;
    let range_cache = lua.create_table()?;
    range_cache.set("start_row", 0i64)?;
    range_cache.set("end_row", 0i64)?;
    range_cache.set("items", lua.create_table()?)?;
    self_table.set("range_cache", range_cache)?;
    Ok(())
}

/// Attach project info to an item table from project_roots.
fn attach_project(item: &LuaTable, project_roots: &LuaTable) -> LuaResult<()> {
    let project_root: Option<String> = item.get("project_root")?;
    if let Some(root) = project_root {
        let project: LuaValue = project_roots.get(root.as_str())?;
        item.set("project", project)?;
        let name: Option<String> = item.get("name")?;
        if let Some(n) = name {
            item.set("filename", n)?;
        }
    }
    Ok(())
}

/// Fetch a single row; reads model_roots and project_roots from self.
fn get_item_by_row(lua: &Lua, (self_table, row): (LuaTable, i64)) -> LuaResult<LuaValue> {
    let visible_count: i64 = self_table.get("visible_count")?;
    if row < 1 || row > visible_count {
        return Ok(LuaValue::Nil);
    }
    let range_cache: LuaTable = self_table.get("range_cache")?;
    let cache_start: i64 = range_cache.get("start_row")?;
    let cache_end: i64 = range_cache.get("end_row")?;
    if row >= cache_start && row <= cache_end && cache_start > 0 {
        let items: LuaTable = range_cache.get("items")?;
        let idx = row - cache_start + 1;
        return items.raw_get(idx);
    }
    let model_roots: LuaTable = self_table.get("model_roots")?;
    let project_roots: LuaTable = self_table.get("project_roots")?;
    let tree_model = require_table(lua, "tree_model")?;
    let item_at: LuaFunction = tree_model.get("item_at")?;
    let item: LuaValue = item_at.call((model_roots, row))?;
    if let LuaValue::Table(ref t) = item {
        attach_project(t, &project_roots)?;
    }
    Ok(item)
}

/// Fetch a range of rows; reads model_roots and project_roots from self.
fn get_items_in_range(
    lua: &Lua,
    (self_table, start_row, end_row): (LuaTable, i64, i64),
) -> LuaResult<LuaTable> {
    if start_row < 1 || end_row < start_row {
        return lua.create_table();
    }
    let visible_count: i64 = self_table.get("visible_count")?;
    let start_row = start_row.max(1).min(visible_count);
    let end_row = end_row.max(1).min(visible_count);
    let range_cache: LuaTable = self_table.get("range_cache")?;
    let cache_start: i64 = range_cache.get("start_row")?;
    let cache_end: i64 = range_cache.get("end_row")?;
    if start_row == cache_start && end_row == cache_end {
        return range_cache.get("items");
    }
    let model_roots: LuaTable = self_table.get("model_roots")?;
    let project_roots: LuaTable = self_table.get("project_roots")?;
    let tree_model = require_table(lua, "tree_model")?;
    let items_in_range: LuaFunction = tree_model.get("items_in_range")?;
    let items: LuaTable = items_in_range.call((model_roots, start_row, end_row))?;
    for item in items.clone().sequence_values::<LuaTable>() {
        let item = item?;
        attach_project(&item, &project_roots)?;
    }
    range_cache.set("start_row", start_row)?;
    range_cache.set("end_row", end_row)?;
    range_cache.set("items", items.clone())?;
    Ok(items)
}

/// Apply font-derived scale metrics and reset label caches.
///
/// All metric values are computed by the Lua caller using font userdata calls
/// that Rust cannot make directly.
fn apply_scale_metrics(
    lua: &Lua,
    (self_table, icon_width, chevron_width, text_spacing, font_height, icon_font_height): (
        LuaTable,
        f64,
        f64,
        f64,
        f64,
        f64,
    ),
) -> LuaResult<()> {
    self_table.set("item_icon_width", icon_width)?;
    self_table.set("item_chevron_width", chevron_width)?;
    self_table.set("item_text_spacing", text_spacing)?;
    self_table.set("item_font_height", font_height)?;
    self_table.set("icon_font_height", icon_font_height)?;
    self_table.set("text_width_cache", lua.create_table()?)?;
    self_table.set("label_cache", lua.create_table()?)?;
    self_table.set("label_cache_count", 0i64)?;
    Ok(())
}

/// Update hover/tooltip state from mouse-move coordinates.
///
/// `item` is nil when the cursor is outside any row; `in_text_box` indicates
/// whether the cursor overlaps the item label (computed in Lua via font calls).
/// `cur_time` is `system.get_time()` passed from Lua so Rust avoids that call.
fn update_hover(
    _lua: &Lua,
    (self_table, item, in_text_box, px, py, same_hover, cur_time): (
        LuaTable,
        LuaValue,
        bool,
        f64,
        f64,
        bool,
        f64,
    ),
) -> LuaResult<()> {
    let tooltip: LuaTable = self_table.get("tooltip")?;
    match &item {
        LuaValue::Table(t) => {
            let abs_filename: LuaValue = t.get("abs_filename")?;
            self_table.set("hovered_item", item.clone())?;
            self_table.set("hovered_path", abs_filename)?;
            if in_text_box {
                tooltip.set("x", px)?;
                tooltip.set("y", py)?;
                if !same_hover {
                    tooltip.set("begin", cur_time)?;
                }
            } else {
                tooltip.set("x", LuaValue::Nil)?;
                tooltip.set("y", LuaValue::Nil)?;
            }
        }
        _ => {
            self_table.set("hovered_item", LuaValue::Nil)?;
            self_table.set("hovered_path", LuaValue::Nil)?;
            tooltip.set("x", LuaValue::Nil)?;
            tooltip.set("y", LuaValue::Nil)?;
        }
    }
    Ok(())
}

/// Update selected_item/selected_path and compute scroll target.
///
/// `item_height` is computed by the Lua caller via `self:get_item_height()` so
/// Rust never needs to touch the font userdata.
fn set_selection(
    _lua: &Lua,
    (self_table, selection, selection_y_val, center, instant, item_height): (
        LuaTable,
        LuaValue,
        LuaValue,
        Option<bool>,
        Option<bool>,
        f64,
    ),
) -> LuaResult<()> {
    // Lua uses `false` (not nil) to signal "no scroll", so handle both.
    let selection_y: Option<f64> = match selection_y_val {
        LuaValue::Number(n) => Some(n),
        LuaValue::Integer(n) => Some(n as f64),
        _ => None,
    };
    match &selection {
        LuaValue::Table(t) => {
            let abs_filename: Option<String> = t.get("abs_filename")?;
            self_table.set("selected_item", selection.clone())?;
            self_table.set("selected_path", abs_filename.clone())?;
            if let Some(sel_y) = selection_y {
                let size: LuaTable = self_table.get("size")?;
                let size_y: f64 = size.get("y")?;
                let lh = item_height.max(1.0);
                if sel_y <= 0.0 || sel_y >= size_y {
                    let mut scroll_y = sel_y;
                    let is_center = center.unwrap_or(false);
                    if !is_center && sel_y >= size_y - lh {
                        scroll_y = sel_y - size_y + lh;
                    }
                    if is_center {
                        scroll_y = sel_y - (size_y - lh) / 2.0;
                    }
                    let scroll: LuaTable = self_table.get("scroll")?;
                    let scroll_to: LuaTable = scroll.get("to")?;
                    let count_lines: i64 = self_table.get("count_lines").unwrap_or(0);
                    let max_scroll = (count_lines as f64 + 1.0) * lh - size_y;
                    let scroll_val = scroll_y.max(0.0).min(max_scroll.max(0.0));
                    scroll_to.set("y", scroll_val)?;
                    if instant.unwrap_or(false) {
                        scroll.set("y", scroll_val)?;
                    }
                }
            }
        }
        LuaValue::Nil => {
            self_table.set("selected_item", LuaValue::Nil)?;
            self_table.set("selected_path", LuaValue::Nil)?;
        }
        _ => {}
    }
    Ok(())
}

fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    let m = lua.create_table()?;
    m.set("init", lua.create_function(|lua, self_table: LuaTable| init(lua, self_table))?)?;
    m.set("sync_model", lua.create_function(|lua, self_table: LuaTable| sync_model(lua, self_table))?)?;
    m.set("get_item_by_row", lua.create_function(get_item_by_row)?)?;
    m.set("get_items_in_range", lua.create_function(get_items_in_range)?)?;
    m.set("apply_scale_metrics", lua.create_function(apply_scale_metrics)?)?;
    m.set("update_hover", lua.create_function(update_hover)?)?;
    m.set("set_selection", lua.create_function(set_selection)?)?;
    Ok(m)
}

pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let package: LuaTable = lua.globals().get("package")?;
    let preload: LuaTable = package.get("preload")?;
    let native_key = lua.create_registry_value(make_module(lua)?)?;
    preload.set(
        "treeview_native",
        lua.create_function(move |lua, ()| lua.registry_value::<LuaTable>(&native_key))?,
    )?;
    preload.set(
        "plugins.treeview",
        lua.create_function(|lua, ()| {
            lua.load(BOOTSTRAP)
                .set_name("plugins.treeview")
                .eval::<LuaValue>()
        })?,
    )?;
    Ok(())
}
