use mlua::prelude::*;

const BOOTSTRAP: &str = r#"local core = require "core"
local common = require "core.common"
local style = require "core.style"
local Node = require "core.node"
local View = require "core.view"
local DocView = require "core.docview"
local EmptyView = require "core.emptyview"
local ContextMenu = require "core.contextmenu"
local native_root_model = require "root_model"
local native_rootview = require "rootview_native"

---@class core.rootview : core.view
---@field super core.view
---@field root_node core.node
---@field mouse core.view.position
local RootView = View:extend()
local root_split_type = {
  left = "hsplit",
  right = "hsplit",
  top = "vsplit",
  bottom = "vsplit",
}

local function collect_docviews(node, out, set)
  out = out or {}
  set = set or {}

  if node.type == "leaf" then
    for _, view in ipairs(node.views or {}) do
      if view:is(DocView) and not set[view] then
        out[#out + 1] = view
        set[view] = true
      end
    end
    return out, set
  end

  collect_docviews(node.a, out, set)
  collect_docviews(node.b, out, set)
  return out, set
end

local function serialize_node_ids(node, view_to_id)
  local state = {
    type = node.type,
    divider = node.divider,
    locked = node.locked,
    resizable = node.resizable,
    is_primary_node = node.is_primary_node,
  }

  if node.type == "leaf" then
    state.views = {}
    state.active_view = node.active_view and view_to_id[node.active_view] or nil
    state.tab_offset = node.tab_offset
    for _, view in ipairs(node.views or {}) do
      if not view:is(EmptyView) then
        state.views[#state.views + 1] = {
          id = view_to_id[view],
          doc = view:is(DocView),
        }
      end
    end
  else
    state.a = serialize_node_ids(node.a, view_to_id)
    state.b = serialize_node_ids(node.b, view_to_id)
  end

  return state
end

local function build_view_maps(node, view_to_id, views_by_id, next_id)
  view_to_id = view_to_id or setmetatable({}, { __mode = "k" })
  views_by_id = views_by_id or {}
  next_id = next_id or 1

  if node.type == "leaf" then
    for _, view in ipairs(node.views or {}) do
      if not view:is(EmptyView) and not view_to_id[view] then
        view_to_id[view] = next_id
        views_by_id[next_id] = view
        next_id = next_id + 1
      end
    end
  else
    view_to_id, views_by_id, next_id = build_view_maps(node.a, view_to_id, views_by_id, next_id)
    view_to_id, views_by_id, next_id = build_view_maps(node.b, view_to_id, views_by_id, next_id)
  end

  return view_to_id, views_by_id, next_id
end

local function collect_live_view_ids(node, view_to_id, only_docviews, out, set)
  out = out or {}
  set = set or {}
  if node.type == "leaf" then
    for _, view in ipairs(node.views or {}) do
      if not view:is(EmptyView)
          and (not only_docviews or view:is(DocView)) then
        local id = view_to_id[view]
        if id and not set[id] then
          out[#out + 1] = id
          set[id] = true
        end
      end
    end
    return out, set
  end
  collect_live_view_ids(node.a, view_to_id, only_docviews, out, set)
  collect_live_view_ids(node.b, view_to_id, only_docviews, out, set)
  return out, set
end

local function restore_node_from_ids(state, views_by_id)
  local node = Node(state.type == "leaf" and nil or state.type)
  node.is_primary_node = state.is_primary_node

  if state.type == "leaf" then
    node.views = {}
    node.active_view = nil

    for _, view_id in ipairs(state.views or {}) do
      local view = views_by_id[view_id]
      if view then
        node:add_view(view)
      end
    end

    if #node.views == 0 then
      node:add_view(EmptyView())
    else
      local active_view = state.active_view and views_by_id[state.active_view] or nil
      if active_view and node:get_view_idx(active_view) then
        node:set_active_view(active_view)
      end
    end

    node.tab_offset = common.clamp(state.tab_offset or 1, 1, math.max(#node.views, 1))
    node.locked = state.locked
    node.resizable = state.resizable
    return node
  end

  node.a = restore_node_from_ids(state.a, views_by_id)
  node.b = restore_node_from_ids(state.b, views_by_id)
  node.divider = state.divider or 0.5
  node.locked = state.locked
  node.resizable = state.resizable
  return node
end

local function get_edge_node(root, placement)
  local target = (placement == "left" or placement == "top") and "a" or "b"
  local split_type = root_split_type[placement]
  if root.type == split_type and root[target] and root[target].type == "leaf" and not root[target].locked then
    return root[target]
  end
end

function RootView:__tostring() return "RootView" end

function RootView:new()
  RootView.super.new(self)
  self.root_node = Node()
  self.deferred_draws = {}
  self.mouse = { x = 0, y = 0 }
  self.drag_overlay = { x = 0, y = 0, w = 0, h = 0, visible = false, opacity = 0,
                        base_color = style.drag_overlay,
                        color = { table.unpack(style.drag_overlay) } }
  self.drag_overlay.to = { x = 0, y = 0, w = 0, h = 0 }
  self.drag_overlay_tab = { x = 0, y = 0, w = 0, h = 0, visible = false, opacity = 0,
                            base_color = style.drag_overlay_tab,
                            color = { table.unpack(style.drag_overlay_tab) } }
  self.drag_overlay_tab.to = { x = 0, y = 0, w = 0, h = 0 }
  self.grab = nil -- = {view = nil, button = nil}
  self.overlapping_view = nil
  self.touched_view = nil
  self.defer_open_docs = {}
  self.first_dnd_processed = false
  self.first_update_done = false
  self.context_menu = ContextMenu()
  self.focus_mode = nil
end


function RootView:defer_draw(fn, ...)
  table.insert(self.deferred_draws, 1, { fn = fn, ... })
end


---@return core.node
function RootView:get_active_node()
  local node = self.root_node:get_node_for_view(core.active_view)
  if not node then node = self:get_primary_node() end
  return node
end


---@return core.node
local function get_primary_node(node)
  if node.is_primary_node then
    return node
  end
  if node.type ~= "leaf" then
    return get_primary_node(node.a) or get_primary_node(node.b)
  end
end


---@return core.node
function RootView:get_active_node_default()
  local node = self.root_node:get_node_for_view(core.active_view)
  if not node then node = self:get_primary_node() end
  if node.locked then
    local default_view = self:get_primary_node().views[1]
    assert(default_view, "internal error: cannot find original document node.")
    core.set_active_view(default_view)
    node = self:get_active_node()
  end
  return node
end


---@return core.node
function RootView:get_primary_node()
  return get_primary_node(self.root_node)
end


---@param node core.node
---@return core.node
local function select_next_primary_node(node)
  if node.is_primary_node then return end
  if node.type ~= "leaf" then
    return select_next_primary_node(node.a) or select_next_primary_node(node.b)
  else
    local lx, ly = node:get_locked_size()
    if not lx and not ly then
      return node
    end
  end
end


---@return core.node
function RootView:select_next_primary_node()
  return select_next_primary_node(self.root_node)
end


---@param doc core.doc
---@return core.docview
function RootView:open_doc(doc)
  local node = self:get_active_node_default()
  for i, view in ipairs(node.views) do
    if view.doc == doc then
      node:set_active_view(node.views[i])
      return view
    end
  end
  local view = DocView(doc)
  node:add_view(view)
  self.root_node:update_layout()
  view:scroll_to_line(view.doc:get_selection(), true, true)
  return view
end


function RootView:add_view(view, placement)
  placement = placement or "tab"
  if self.focus_mode and (placement ~= "tab" or not view:is(DocView)) then
    self:exit_focus_mode()
  end
  if placement == "tab" then
    self:get_active_node_default():add_view(view)
    self.root_node:update_layout()
    core.set_active_view(view)
    return view
  end

  local edge = get_edge_node(self.root_node, placement)
  if edge then
    edge:add_view(view)
    self.root_node:update_layout()
    core.set_active_view(view)
    return view
  end

  local split_type = assert(root_split_type[placement], "invalid root placement")
  local existing = Node()
  existing:consume(self.root_node)

  local sibling = Node()
  sibling.views = {}
  sibling:add_view(view)

  local new_root = Node(split_type)
  new_root.a = existing
  new_root.b = sibling
  if placement == "left" or placement == "top" then
    new_root.a, new_root.b = new_root.b, new_root.a
  end

  self.root_node:consume(new_root)
  self.root_node:update_layout()
  core.set_active_view(view)
  return view
end

local function is_session_view(view)
  return view and view.context == "session"
end

function RootView:get_session_views()
  local views = {}
  local function walk(node)
    if node.type == "leaf" then
      for _, view in ipairs(node.views or {}) do
        if is_session_view(view) and not view:is(EmptyView) then
          views[#views + 1] = { node = node, view = view }
        end
      end
    else
      walk(node.a)
      walk(node.b)
    end
  end
  walk(self.root_node)
  return views
end

function RootView:is_focus_mode_active()
  return self.focus_mode ~= nil
end

function RootView:enter_focus_mode()
  local active_view = core.active_view
  if not (active_view and active_view:is(DocView)) then
    return false
  end
  if self.focus_mode then
    return true
  end

  local focus_views = collect_docviews(self.root_node)
  if #focus_views == 0 then
    return false
  end

  local focus_root = Node()
  focus_root.views = {}
  focus_root.active_view = nil
  focus_root.is_primary_node = true

  for _, view in ipairs(focus_views) do
    focus_root:add_view(view)
  end

  if focus_root:get_view_idx(active_view) then
    focus_root:set_active_view(active_view)
  end

  self.focus_mode = {
    view_to_id = nil,
    views_by_id = nil,
    snapshot_ids = nil,
    previous_active_view = active_view,
    previous_active_view_id = nil,
  }
  local view_to_id, views_by_id = build_view_maps(self.root_node)
  self.focus_mode.view_to_id = view_to_id
  self.focus_mode.views_by_id = views_by_id
  self.focus_mode.snapshot_ids = serialize_node_ids(self.root_node, view_to_id)
  self.focus_mode.previous_active_view_id = active_view and view_to_id[active_view] or nil
  self.root_node:consume(focus_root)
  self.root_node:update_layout()
  core.redraw = true
  return true
end

function RootView:exit_focus_mode()
  local focus_state = self.focus_mode
  if not focus_state then
    return false
  end

  local restored_root
  local live_doc_ids = collect_live_view_ids(self.root_node, focus_state.view_to_id, true)
  local live_view_ids = collect_live_view_ids(self.root_node, focus_state.view_to_id, false)
  local current_active_id = core.active_view and focus_state.view_to_id[core.active_view] or nil
  local restored = native_root_model.restore_focus_layout(
    focus_state.snapshot_ids,
    live_doc_ids,
    live_view_ids,
    current_active_id,
    focus_state.previous_active_view_id
  )
  restored_root = restore_node_from_ids(restored.root, focus_state.views_by_id)
  local target_view = restored.target_view_id and focus_state.views_by_id[restored.target_view_id] or nil

  self.focus_mode = nil
  self.root_node:consume(restored_root)
  self.root_node:update_layout()

  target_view = target_view or core.active_view
  if not self.root_node:get_node_for_view(target_view) then
    target_view = focus_state.previous_active_view
  end
  local target_node = target_view and self.root_node:get_node_for_view(target_view) or self:get_primary_node()
  if target_node then
    target_node:set_active_view(target_view or target_node.active_view or target_node.views[1])
  end

  core.redraw = true
  return true
end

function RootView:toggle_focus_mode()
  if self.focus_mode then
    return self:exit_focus_mode()
  end
  return self:enter_focus_mode()
end

function RootView:close_views(entries)
  for i = #entries, 1, -1 do
    local entry = entries[i]
    if entry.node and entry.view and entry.node:get_view_idx(entry.view) then
      if entry.view.doc then
        entry.node:remove_view(self.root_node, entry.view)
      else
        entry.view:try_close(function()
          if entry.node:get_view_idx(entry.view) then
            entry.node:remove_view(self.root_node, entry.view)
          end
        end)
      end
    end
  end
  self.root_node:update_layout()
end

function RootView:confirm_close_views(entries)
  local docs = {}
  local seen = {}
  for _, entry in ipairs(entries) do
    local doc = entry.view and entry.view.doc
    if doc and doc:is_dirty() and not seen[doc] then
      seen[doc] = true
      docs[#docs + 1] = doc
    end
  end
  local function do_close()
    self:close_views(entries)
  end
  if #docs > 0 then
    core.confirm_close_docs(docs, do_close)
  else
    do_close()
  end
end

function RootView:show_tab_context_menu(node, idx, x, y)
  local view = node.views[idx]
  if not view then
    return false
  end
  local right = {}
  for _, entry in ipairs(node:get_views_to_right(view)) do
    right[#right + 1] = { node = node, view = entry }
  end
  local all = self:get_session_views()
  local others = {}
  local saved = {}
  for _, entry in ipairs(all) do
    if entry.view ~= view then
      others[#others + 1] = entry
    end
    if entry.view.doc and not entry.view.doc:is_dirty() then
      saved[#saved + 1] = entry
    end
  end
  local items = {
    { text = "Close", command = function() self:confirm_close_views({ { node = node, view = view } }) end },
    { text = "Close Right", command = function() self:confirm_close_views(right) end },
    { text = "Close Others", command = function() self:confirm_close_views(others) end },
    { text = "Close Saved", command = function() self:confirm_close_views(saved) end },
    { text = "Close All", command = function() self:confirm_close_views(all) end },
  }
  return self.context_menu:show(x, y, items)
end


---@param keep_active boolean
function RootView:close_all_docviews(keep_active)
  self.root_node:close_all_docviews(keep_active)
end


---Obtain mouse grab.
---
---This means that mouse movements will be sent to the specified view, even when
---those occur outside of it.
---There can't be multiple mouse grabs, even for different buttons.
---@see RootView:ungrab_mouse
---@param button core.view.mousebutton
---@param view core.view
function RootView:grab_mouse(button, view)
  assert(self.grab == nil)
  self.grab = {view = view, button = button}
end


---Release mouse grab.
---
---The specified button *must* be the last button that grabbed the mouse.
---@see RootView:grab_mouse
---@param button core.view.mousebutton
function RootView:ungrab_mouse(button)
  assert(self.grab and self.grab.button == button)
  self.grab = nil
end


---Function to intercept mouse pressed events on the active view.
---Do nothing by default.
---@param button core.view.mousebutton
---@param x number
---@param y number
---@param clicks integer
function RootView.on_view_mouse_pressed(button, x, y, clicks)
end


---@param button core.view.mousebutton
---@param x number
---@param y number
---@param clicks integer
---@return boolean
function RootView:on_mouse_pressed(button, x, y, clicks)
  -- If there is a grab, release it first
  if self.grab then
    self:on_mouse_released(self.grab.button, x, y)
  end
  if self.context_menu:on_mouse_pressed(button, x, y, clicks) then
    return true
  end
  local div = self.root_node:get_divider_overlapping_point(x, y)
  local node = self.root_node:get_child_overlapping_point(x, y)
  if div and (node and not node.active_view:scrollbar_overlaps_point(x, y)) then
    self.dragged_divider = div
    return true
  end
  if node.hovered_scroll_button > 0 then
    node:scroll_tabs(node.hovered_scroll_button)
    return true
  end
  local idx = node:get_tab_overlapping_point(x, y)
  if idx then
    if button == "right" then
      node:set_active_view(node.views[idx])
      return self:show_tab_context_menu(node, idx, x, y)
    end
    if button == "middle" or node.hovered_close == idx then
      node:close_view(self.root_node, node.views[idx])
      return true
    else
      if button == "left" then
        self.dragged_node = { node = node, idx = idx, dragging = false, drag_start_x = x, drag_start_y = y}
      end
      node:set_active_view(node.views[idx])
      return true
    end
  elseif not self.dragged_node then -- avoid sending on_mouse_pressed events when dragging tabs
    core.set_active_view(node.active_view)
    self:grab_mouse(button, node.active_view)
    return self.on_view_mouse_pressed(button, x, y, clicks) or node.active_view:on_mouse_pressed(button, x, y, clicks)
  end
end


function RootView:get_overlay_base_color(overlay)
  if overlay == self.drag_overlay then
    return style.drag_overlay
  else
    return style.drag_overlay_tab
  end
end


function RootView:set_show_overlay(overlay, status)
  overlay.visible = status
  if status then -- reset colors
    -- reload base_color
    overlay.base_color = self:get_overlay_base_color(overlay)
    overlay.color[1] = overlay.base_color[1]
    overlay.color[2] = overlay.base_color[2]
    overlay.color[3] = overlay.base_color[3]
    overlay.color[4] = overlay.base_color[4]
    overlay.opacity = 0
  end
end


---@param button core.view.mousebutton
---@param x number
---@param y number
function RootView:on_mouse_released(button, x, y, ...)
  if self.grab then
    if self.grab.button == button then
      local grabbed_view = self.grab.view
      grabbed_view:on_mouse_released(button, x, y, ...)
      self:ungrab_mouse(button)

      -- If the mouse was released over a different view, send it the mouse position
      local hovered_view = self.root_node:get_child_overlapping_point(x, y)
      if grabbed_view ~= hovered_view then
        self:on_mouse_moved(x, y, 0, 0)
      end
    end
    return
  end

  if self.context_menu:on_mouse_released(button, x, y, ...) then
    return true
  end
  if self.dragged_divider then
    self.dragged_divider = nil
  end
  if self.dragged_node then
    if button == "left" then
      if self.dragged_node.dragging then
        local node = self.root_node:get_child_overlapping_point(self.mouse.x, self.mouse.y)
        local dragged_node = self.dragged_node.node

        if node and not node.locked
           -- don't do anything if dragging onto own node, with only one view
           and (node ~= dragged_node or #node.views > 1) then
          local split_type = node:get_split_type(self.mouse.x, self.mouse.y)
          local view = dragged_node.views[self.dragged_node.idx]

          if split_type ~= "middle" and split_type ~= "tab" then -- needs splitting
            local new_node = node:split(split_type)
            self.root_node:get_node_for_view(view):remove_view(self.root_node, view)
            new_node:add_view(view)
          elseif split_type == "middle" and node ~= dragged_node then -- move to other node
            dragged_node:remove_view(self.root_node, view)
            node:add_view(view)
            self.root_node:get_node_for_view(view):set_active_view(view)
          elseif split_type == "tab" then -- move besides other tabs
            local tab_index = node:get_drag_overlay_tab_position(self.mouse.x, self.mouse.y, dragged_node, self.dragged_node.idx)
            dragged_node:remove_view(self.root_node, view)
            node:add_view(view, tab_index)
            self.root_node:get_node_for_view(view):set_active_view(view)
          end
          self.root_node:update_layout()
          core.redraw = true
        end
      end
      self:set_show_overlay(self.drag_overlay, false)
      self:set_show_overlay(self.drag_overlay_tab, false)
      if self.dragged_node and self.dragged_node.dragging then
        core.request_cursor("arrow")
      end
      self.dragged_node = nil
    end
  end
end


local function resize_child_node(node, axis, value, delta)
  local accept_resize = node.a:resize(axis, value)
  if not accept_resize then
    accept_resize = node.b:resize(axis, node.size[axis] - value)
  end
  if not accept_resize then
    node.divider = node.divider + delta / node.size[axis]
  end
end


---@param x number
---@param y number
---@param dx number
---@param dy number
function RootView:on_mouse_moved(x, y, dx, dy)
  self.mouse.x, self.mouse.y = x, y

  if self.grab then
    self.grab.view:on_mouse_moved(x, y, dx, dy)
    core.request_cursor(self.grab.view.cursor)
    return
  end

  if self.context_menu:on_mouse_moved(x, y, dx, dy) then
    return true
  end

  if core.active_view == core.nag_view then
    core.request_cursor("arrow")
    core.active_view:on_mouse_moved(x, y, dx, dy)
    return
  end

  if self.dragged_divider then
    local node = self.dragged_divider
    if node.type == "hsplit" then
      x = common.clamp(x - node.position.x, 0, self.root_node.size.x * 0.95)
      resize_child_node(node, "x", x, dx)
    elseif node.type == "vsplit" then
      y = common.clamp(y - node.position.y, 0, self.root_node.size.y * 0.95)
      resize_child_node(node, "y", y, dy)
    end
    node.divider = common.clamp(node.divider, 0.01, 0.99)
    return
  end

  local dn = self.dragged_node
  if dn and not dn.dragging then
    -- start dragging only after enough movement
    dn.dragging = common.distance(x, y, dn.drag_start_x, dn.drag_start_y) > style.tab_width * .05
    if dn.dragging then
      core.request_cursor("hand")
    end
  end

  -- avoid sending on_mouse_moved events when dragging tabs
  if dn then return end

  local last_overlapping_view = self.overlapping_view
  local overlapping_node = self.root_node:get_child_overlapping_point(x, y)
  self.overlapping_view = overlapping_node and overlapping_node.active_view

  if last_overlapping_view and last_overlapping_view ~= self.overlapping_view then
    last_overlapping_view:on_mouse_left()
  end

  if not self.overlapping_view then return end

  self.overlapping_view:on_mouse_moved(x, y, dx, dy)
  core.request_cursor(self.overlapping_view.cursor)

  if not overlapping_node then return end

  local div = self.root_node:get_divider_overlapping_point(x, y)
  if overlapping_node:get_scroll_button_index(x, y) or overlapping_node:is_in_tab_area(x, y) then
    core.request_cursor("arrow")
  elseif div and not self.overlapping_view:scrollbar_overlaps_point(x, y) then
    core.request_cursor(div.type == "hsplit" and "sizeh" or "sizev")
  end
end


function RootView:on_mouse_left()
  if self.overlapping_view then
    self.overlapping_view:on_mouse_left()
  end
end


---@param filename string
---@param x number
---@param y number
---@return boolean
function RootView:on_file_dropped(filename, x, y)
  local node = self.root_node:get_child_overlapping_point(x, y)
  local result = node and node.active_view:on_file_dropped(filename, x, y)
  if result then return result end
  local info = system.get_file_info(filename)
  if info and info.type == "dir" then
    local abspath = system.absolute_path(filename) --[[@as string]]
    if self.first_update_done then
      -- ask the user if they want to open it here or somewhere else
      core.nag_view:show(
        "Open directory",
        string.format('You are trying to open "%s"\n', common.home_encode(abspath))
        .. "Do you want to open this directory here, or in a new window?",
        {
          { text = "Current window", default_yes = true },
          { text = "New window", default_no = true },
          { text = "Cancel" }
        },
        function(opt)
          if opt.text == "Current window" then
            core.add_project(abspath)
          elseif opt.text == "New window" then
            system.exec(string.format("%q %q", EXEFILE, filename))
          end
        end
      )
      return true
    end
    -- in macOS, when dropping folders into Lite-Anvil in the dock,
    -- the OS tries to start an instance of Lite-Anvil with each folder as a DND request.
    -- When this happens, the DND request always arrive before the first update() call.
    -- We need to change the current project folder for the first request, and start
    -- new instances for the rest to emulate existing behavior.
    if self.first_dnd_processed then
      -- FIXME: port to process API
      system.exec(string.format("%q %q", EXEFILE, filename))
    else
      -- change project directory
      core.confirm_close_docs(core.docs, function(dirpath)
        core.open_folder_project(dirpath)
      end, system.absolute_path(filename))
      self.first_dnd_processed = true
    end
    return true
  end
  -- defer opening docs in case nagview is visible (which will cause a locked node error)
  table.insert(self.defer_open_docs, { filename, x, y })
  return true
end

function RootView:process_defer_open_docs()
  if core.active_view == core.nag_view then return end
  for _, drop in ipairs(self.defer_open_docs) do
    -- file dragged into lite-anvil, try to open it
    local filename, x, y = table.unpack(drop)
    local ok, doc = core.try(core.open_doc, filename)
    if ok then
      local node = core.root_view.root_node:get_child_overlapping_point(x, y)
      node:set_active_view(node.active_view)
      core.root_view:open_doc(doc)
    end
  end
  self.defer_open_docs = {}
end


function RootView:on_mouse_wheel(...)
  local x, y = self.mouse.x, self.mouse.y
  local node = self.root_node:get_child_overlapping_point(x, y)
  return node.active_view:on_mouse_wheel(...)
end


function RootView:on_text_input(...)
  core.active_view:on_text_input(...)
end

function RootView:on_touch_pressed(x, y, ...)
  local touched_node = self.root_node:get_child_overlapping_point(x, y)
  self.touched_view = touched_node and touched_node.active_view
end

function RootView:on_touch_released(x, y, ...)
  self.touched_view = nil
end

function RootView:on_touch_moved(x, y, dx, dy, ...)
  if not self.touched_view then return end
  if core.active_view == core.nag_view then
    core.active_view:on_touch_moved(x, y, dx, dy, ...)
    return
  end

  if self.dragged_divider then
    local node = self.dragged_divider
    if node.type == "hsplit" then
      x = common.clamp(x - node.position.x, 0, self.root_node.size.x * 0.95)
      resize_child_node(node, "x", x, dx)
    elseif node.type == "vsplit" then
      y = common.clamp(y - node.position.y, 0, self.root_node.size.y * 0.95)
      resize_child_node(node, "y", y, dy)
    end
    node.divider = common.clamp(node.divider, 0.01, 0.99)
    return
  end

  local dn = self.dragged_node
  if dn and not dn.dragging then
    -- start dragging only after enough movement
    dn.dragging = common.distance(x, y, dn.drag_start_x, dn.drag_start_y) > style.tab_width * .05
    if dn.dragging then
      core.request_cursor("hand")
    end
  end

  -- avoid sending on_touch_moved events when dragging tabs
  if dn then return end

  self.touched_view:on_touch_moved(x, y, dx, dy, ...)
end

function RootView:on_ime_text_editing(...)
  core.active_view:on_ime_text_editing(...)
end

function RootView:on_focus_lost(...)
  -- We force a redraw so documents can redraw without the cursor.
  core.redraw = true
end

function RootView:on_focus_gained(...)
end


function RootView:interpolate_drag_overlay(overlay)
  self:move_towards(overlay, "x", overlay.to.x, nil, "tab_drag")
  self:move_towards(overlay, "y", overlay.to.y, nil, "tab_drag")
  self:move_towards(overlay, "w", overlay.to.w, nil, "tab_drag")
  self:move_towards(overlay, "h", overlay.to.h, nil, "tab_drag")

  self:move_towards(overlay, "opacity", overlay.visible and 100 or 0, nil, "tab_drag")
  overlay.color[4] = overlay.base_color[4] * overlay.opacity / 100
end


function RootView:update()
  Node.copy_position_and_size(self.root_node, self)
  self.root_node:update()
  self.root_node:update_layout()

  self:update_drag_overlay()
  self:interpolate_drag_overlay(self.drag_overlay)
  self:interpolate_drag_overlay(self.drag_overlay_tab)
  self:process_defer_open_docs()
  self.first_update_done = true
  self.context_menu:update()
  -- set this to true because at this point there are no dnd requests
  -- that are caused by the initial dnd into dock user action
  self.first_dnd_processed = true
end


function RootView:set_drag_overlay(overlay, x, y, w, h, immediate)
  overlay.to.x = x
  overlay.to.y = y
  overlay.to.w = w
  overlay.to.h = h
  if immediate then
    overlay.x = x
    overlay.y = y
    overlay.w = w
    overlay.h = h
  end
  if not overlay.visible then
    self:set_show_overlay(overlay, true)
  end
end


function RootView:update_drag_overlay()
  if not (self.dragged_node and self.dragged_node.dragging) then return end
  local over = self.root_node:get_child_overlapping_point(self.mouse.x, self.mouse.y)
  if over and not over.locked then
    local _, _, _, tab_h = over:get_scroll_button_rect(1)
    local x, y = over.position.x, over.position.y
    local w, h = over.size.x, over.size.y
    local split_type = over:get_split_type(self.mouse.x, self.mouse.y)

    if split_type == "tab" and (over ~= self.dragged_node.node or #over.views > 1) then
      local tab_index, tab_x, tab_y, tab_w, tab_h = over:get_drag_overlay_tab_position(self.mouse.x, self.mouse.y)
      self:set_drag_overlay(self.drag_overlay_tab,
        tab_x + (tab_index and 0 or tab_w), tab_y,
        style.caret_width, tab_h,
        -- avoid showing tab overlay moving between nodes
        over ~= self.drag_overlay_tab.last_over)
      self:set_show_overlay(self.drag_overlay, false)
      self.drag_overlay_tab.last_over = over
    else
      if (over ~= self.dragged_node.node or #over.views > 1) then
        y = y + tab_h
        h = h - tab_h
        x, y, w, h = native_rootview.split_rect(split_type, x, y, w, h)
      end
      self:set_drag_overlay(self.drag_overlay, x, y, w, h)
      self:set_show_overlay(self.drag_overlay_tab, false)
    end
  else
    self:set_show_overlay(self.drag_overlay, false)
    self:set_show_overlay(self.drag_overlay_tab, false)
  end
end


function RootView:draw_grabbed_tab()
  local dn = self.dragged_node
  local _,_, w, h = dn.node:get_tab_rect(dn.idx)
  local x = self.mouse.x - w / 2
  local y = self.mouse.y - h / 2
  local view = dn.node.views[dn.idx]
  self.root_node:draw_tab(view, true, true, false, x, y, w, h, true)
end


function RootView:draw_drag_overlay(ov)
  if ov.opacity > 0 then
    renderer.draw_rect(ov.x, ov.y, ov.w, ov.h, ov.color)
  end
end


function RootView:draw()
  self.root_node:draw()
  while #self.deferred_draws > 0 do
    local t = table.remove(self.deferred_draws)
    t.fn(table.unpack(t))
  end

  self:draw_drag_overlay(self.drag_overlay)
  self:draw_drag_overlay(self.drag_overlay_tab)
  if self.dragged_node and self.dragged_node.dragging then
    self:draw_grabbed_tab()
  end
  self.context_menu:draw()
  if core.cursor_change_req then
    system.set_cursor(core.cursor_change_req)
    core.cursor_change_req = nil
  end
end

return RootView
"#;

/// Computes the sub-rectangle for a drag-split overlay based on split direction.
fn split_rect(
    _lua: &Lua,
    (split_type, x, y, w, h): (String, f64, f64, f64, f64),
) -> LuaResult<(f64, f64, f64, f64)> {
    match split_type.as_str() {
        "left" => Ok((x, y, w * 0.5, h)),
        "right" => Ok((x + w * 0.5, y, w * 0.5, h)),
        "up" => Ok((x, y, w, h * 0.5)),
        "down" => Ok((x, y + h * 0.5, w, h * 0.5)),
        _ => Ok((x, y, w, h)),
    }
}

fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    let module = lua.create_table()?;
    module.set("split_rect", lua.create_function(split_rect)?)?;
    Ok(module)
}

/// Registers "rootview_native" (Rust helpers) and "core.rootview" (BOOTSTRAP) as preloads.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let package: LuaTable = lua.globals().get("package")?;
    let preload: LuaTable = package.get("preload")?;

    preload.set(
        "rootview_native",
        lua.create_function(|lua, ()| make_module(lua))?,
    )?;

    preload.set(
        "core.rootview",
        lua.create_function(|lua, ()| {
            lua.load(BOOTSTRAP)
                .set_name("core.rootview")
                .eval::<LuaValue>()
        })?,
    )?;

    Ok(())
}
