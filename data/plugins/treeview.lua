-- mod-version:4
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
local native_tree_model = nil

do
  local ok, mod = pcall(require, "tree_model")
  if ok then native_tree_model = mod end
end

config.plugins.treeview = common.merge({
  -- Default treeview width
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


local function replace_alpha(color, alpha)
  local r, g, b = table.unpack(color)
  return { r, g, b, alpha }
end


local function tree_roots()
  local roots = {}
  for i, project in ipairs(core.projects) do
    roots[i] = project.path
  end
  return roots
end


local function project_by_root(root)
  for _, project in ipairs(core.projects) do
    if project.path == root then
      return project
    end
  end
end


local function tree_model_opts(view)
  return {
    show_hidden = view.show_hidden,
    show_ignored = view.show_ignored,
    max_entries = config.plugins.treeview.max_dir_entries,
    file_size_limit_bytes = config.file_size_limit * 1e6,
    ignore_files = config.ignore_files,
    gitignore_enabled = config.gitignore.enabled ~= false,
    gitignore_additional_patterns = config.gitignore.additional_patterns or {},
  }
end


local TreeView = View:extend()

function TreeView:__tostring() return "TreeView" end

function TreeView:new()
  TreeView.super.new(self)
  self.scrollable = true
  self.visible = config.plugins.treeview.visible
  self.init_size = true
  local saved_size = core.session and core.session.treeview_size
  if type(saved_size) == "number" then
    config.plugins.treeview.size = saved_size
  end
  self.target_size = config.plugins.treeview.size
  self.show_hidden = config.plugins.treeview.show_hidden
  self.show_ignored = config.plugins.treeview.show_ignored
  self.tooltip = { x = 0, y = 0, begin = 0, alpha = 0 }
  self.last_scroll_y = 0
  self.selected_path = nil
  self.hovered_path = nil

  self.item_icon_width = 0
  self.item_text_spacing = 0
  self.items_flat = nil
  self.item_index = {}
  self.items_dirty = true
  self.last_project_count = 0
  self.last_tree_generation = 0
end


function TreeView:set_target_size(axis, value)
  if axis == "x" then
    self.target_size = value
    config.plugins.treeview.size = value
    if core.session then
      core.session.treeview_size = value
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


function TreeView:get_items(project, path, x, y, w, h)
  if self.items_dirty then self:rebuild_items() end
  local count_lines = 0
  for i, item in ipairs(self.items_flat or {}) do
    count_lines = i
    coroutine.yield(item, x, y + (i - 1) * h, w, h)
  end
  return count_lines
end


function TreeView:each_item()
  return coroutine.wrap(function()
    local ox, oy = self:get_content_offset()
    local h = self:get_item_height()
    if self.items_dirty then self:rebuild_items() end
    self.count_lines = #(self.items_flat or {})
    for i, item in ipairs(self.items_flat or {}) do
      coroutine.yield(item, ox, oy + style.padding.y + h * (i - 1), self.size.x, h)
    end
  end)
end


function TreeView:rebuild_items()
  self.items_flat = {}
  self.item_index = {}
  if native_tree_model then
    local roots = tree_roots()
    local opts = tree_model_opts(self)
    native_tree_model.sync_roots(roots, opts)
    self.last_tree_generation = native_tree_model.generation()
    local items = native_tree_model.get_visible_items(roots, opts)
    for i, item in ipairs(items) do
      item.project = project_by_root(item.project_root)
      item.filename = item.name
      self.items_flat[i] = item
      self.item_index[item.abs_filename] = i
    end
  end
  self.count_lines = #self.items_flat
  self.items_dirty = false
  self.last_project_count = #core.projects
end


function TreeView:resolve_path(path)
  if not path then return nil end
  if self.items_dirty then self:rebuild_items() end
  local idx = self.item_index[path]
  if idx then
    local ox, oy = self:get_content_offset()
    local h = self:get_item_height()
    local y = oy + style.padding.y + h * (idx - 1)
    return self.items_flat[idx], ox, y, self.size.x, h
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
  self.selected_item = selection
  self.selected_path = selection and selection.abs_filename or nil
  if selection and selection_y
      and (selection_y <= 0 or selection_y >= self.size.y) then
    local lh = self:get_item_height()
    if not center and selection_y >= self.size.y - lh then
      selection_y = selection_y - self.size.y + lh
    end
    if center then
      selection_y = selection_y - (self.size.y - lh) / 2
    end
    local _, y = self:get_content_offset()
    self.scroll.to.y = selection_y - y
    self.scroll.to.y = common.clamp(self.scroll.to.y, 0, self:get_scrollable_size() - self.size.y)
    if instant then
      self.scroll.y = self.scroll.to.y
    end
  end
end

---Sets the selection to the file with the specified path.
---
---@param path string #Absolute path of item to select
---@param expand boolean #Expand dirs leading to the item
---@param scroll_to boolean #Scroll to make the item visible
---@param instant boolean #Don't animate the scroll
---@return table? #The selected item
function TreeView:set_selection_to_path(path, expand, scroll_to, instant)
  if native_tree_model and expand then
    native_tree_model.expand_to(path)
    self.items_dirty = true
  end
  if self.items_dirty then self:rebuild_items() end
  local to_select, _, to_select_y = self:resolve_path(path)
  if to_select then
    self:set_selection(to_select, scroll_to and to_select_y, true, instant)
  end
  return to_select
end


function TreeView:get_text_bounding_box(item, x, y, w, h)
  local icon_width = style.icon_font:get_width("D")
  local xoffset = item.depth * style.padding.x + style.padding.x + icon_width
  x = x + xoffset
  w = style.font:get_width(item.name) + 2 * style.padding.x
  return x, y, w, h
end



function TreeView:on_mouse_moved(px, py, ...)
  if not self.visible then return end
  if TreeView.super.on_mouse_moved(self, px, py, ...) then
    -- mouse movement handled by the View (scrollbar)
    self.hovered_item = nil
    self.hovered_path = nil
    return
  end

  if self.items_dirty then self:rebuild_items() end

  local ox, oy = self:get_content_offset()
  local h = self:get_item_height()
  local pad = style.padding.y
  local row = math.floor((py - oy - pad) / h)
  local item = (row >= 0 and self.items_flat) and self.items_flat[row + 1]

  local item_changed, tooltip_changed
  if item and px > ox and px <= ox + self.size.x then
    local y = oy + pad + row * h
    item_changed = true
    local same_hover = self.hovered_path == item.abs_filename
    self.hovered_item = item
    self.hovered_path = item.abs_filename

    local ix, iy, iw, ih = self:get_text_bounding_box(item, ox, y, self.size.x, h)
    if px > ix and py > iy and px <= ix + iw and py <= iy + ih then
      tooltip_changed = true
      self.tooltip.x, self.tooltip.y = px, py
      if not same_hover then
        self.tooltip.begin = system.get_time()
      end
    end
  end
  if not item_changed then
    self.hovered_item = nil
    self.hovered_path = nil
  end
  if not tooltip_changed then self.tooltip.x, self.tooltip.y = nil, nil end
end


function TreeView:on_mouse_left()
  TreeView.super.on_mouse_left(self)
  self.hovered_item = nil
  self.hovered_path = nil
end


function TreeView:update()
  -- update width
  local dest = self.visible and self.target_size or 0
  if self.init_size then
    self.size.x = dest
    self.init_size = false
  else
    self:move_towards(self.size, "x", dest, nil, "treeview")
  end

  if self.size.x == 0 or self.size.y == 0 or not self.visible then return end

  local duration = system.get_time() - self.tooltip.begin
  if self.hovered_path and self.tooltip.x and duration > tooltip_delay then
    self:move_towards(self.tooltip, "alpha", tooltip_alpha, tooltip_alpha_rate, "treeview")
  else
    self.tooltip.alpha = 0
  end

  self.item_icon_width = style.icon_font:get_width("D")
  self.item_text_spacing = style.icon_font:get_width("f") / 2

  -- this will make sure hovered_item is updated
  local dy = math.abs(self.last_scroll_y - self.scroll.y)
  if dy > 0 then
    self:on_mouse_moved(core.root_view.mouse.x, core.root_view.mouse.y, 0, 0)
    self.last_scroll_y = self.scroll.y
  end

  local config = config.plugins.treeview
  if native_tree_model and native_tree_model.generation() ~= self.last_tree_generation then
    self.items_dirty = true
  end
  if config.highlight_focused_file then
    -- Try to only highlight when we actually change tabs
    local current_node = core.root_view:get_active_node()
    local current_active_view = core.active_view
    if current_node and not current_node.locked
     and current_active_view ~= self and current_active_view ~= self.last_active_view then
      self.last_active_view = current_active_view
      if DocView:is_extended_by(current_active_view) then
        local abs_filename = current_active_view.doc
                             and current_active_view.doc.abs_filename or ""
        self:set_selection_to_path(abs_filename,
                                   config.expand_dirs_to_focused_file,
                                   config.scroll_to_focused_file,
                                   not config.animate_scroll_to_focused_file)
      else
        self:set_selection(nil)
      end
    end
  end

  TreeView.super.update(self)
end


function TreeView:get_scrollable_size()
  return self.count_lines and self:get_item_height() * (self.count_lines + 1) or math.huge
end


function TreeView:draw_tooltip()
  local hovered = self:get_hovered_item()
  if not hovered then return end
  local text = common.home_encode(hovered.abs_filename)
  local w, h = style.font:get_width(text), style.font:get_height(text)

  local x, y = self.tooltip.x + tooltip_offset, self.tooltip.y + tooltip_offset
  w, h = w + style.padding.x, h + style.padding.y

  if x + w > core.root_view.root_node.size.x then -- check if we can span right
    x = x - w -- span left instead
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
  local text = item.name
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
  common.draw_text(icon_font, icon_color, icon_char, nil, x, y, 0, h)
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
    common.draw_text(style.icon_font, chevron_color, chevron_icon, nil, x, y, 0, h)
  end
  return style.padding.x
end


function TreeView:draw_item_background(item, active, hovered, x, y, w, h)
  if active then
    renderer.draw_rect(x, y, w, h, style.line_highlight)
  end
  if hovered and not active then
    local hover_color = { table.unpack(style.line_highlight) }
    hover_color[4] = 160
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

  if #core.projects ~= self.last_project_count then
    self.items_dirty = true
  end
  if self.items_dirty then self:rebuild_items() end

  local _y, _h = self.position.y, self.size.y
  local ox, oy = self:get_content_offset()
  local h = self:get_item_height()
  local pad = style.padding.y

  for i, item in ipairs(self.items_flat) do
    local y = oy + pad + (i - 1) * h
    if y + h >= _y then
      if y >= _y + _h then break end
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
  if self.items_dirty then self:rebuild_items() end
  local idx = item and self.item_index[item.abs_filename] or nil
  if not idx then
    idx = where >= 0 and 1 or #self.items_flat
  else
    idx = idx + where
  end
  idx = common.clamp(idx, 1, #self.items_flat)
  local target = self.items_flat[idx]
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

  if item.type == "dir" and native_tree_model then
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
  if native_tree_model and project and project.path then
    native_tree_model.invalidate(project.path)
  end
  view.items_dirty = true
end

-- init
view = TreeView()
local node = core.root_view:get_active_node()
view.node = node:split("left", view, {x = true}, true)

-- The toolbarview plugin is special because it is plugged inside
-- a treeview pane which is itelf provided in a plugin.
-- We therefore break the usual plugin's logic that would require each
-- plugin to be independent of each other. In addition it is not the
-- plugin module that plug itself in the active node but it is plugged here
-- in the treeview node.
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
  if native_tree_model then
    native_tree_model.sync_roots(tree_roots(), tree_model_opts(view))
  end
end

local on_quit_project = core.on_quit_project
function core.on_quit_project()
  view.items_dirty = true
  if native_tree_model then
    native_tree_model.clear_all()
  end
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
      -- add secondary project dirs names to the file path to show
      relfilename = common.basename(item.abs_filename) .. PATHSEP .. relfilename
    end
    local file_info = system.get_file_info(filename)
    local file_type = file_info.type == "dir" and "Directory" or "File"
    -- Ask before deleting
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
        if res then -- successfully renamed
          for _, doc in ipairs(core.docs) do
            if doc.abs_filename and old_abs_filename == doc.abs_filename then
              doc:set_filename(filename, abs_filename) -- make doc point to the new filename
              doc:reset_syntax()
              break -- only first needed
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

-- Register the TreeView commands and keymap
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

-- The config specification used by gui generators
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

-- Return the treeview with toolbar and contextmenu to allow
-- user or plugin modifications
view.toolbar = toolbar_view

return view
