-- mod-version:4
local core = require "core"
local common = require "core.common"
local keymap = require "core.keymap"
local command = require "core.command"
local config = require "core.config"
local style = require "core.style"
local View = require "core.view"
local native_project_search = nil
local native_project_fs = nil

do
  local ok, mod = pcall(require, "project_search")
  if ok then native_project_search = mod end
  ok, mod = pcall(require, "project_fs")
  if ok then native_project_fs = mod end
end

---@class plugins.projectsearch.resultsview : core.view
local ResultsView = View:extend()

function ResultsView:__tostring() return "ResultsView" end

ResultsView.context = "session"

local function glob_to_pattern(glob)
  if not glob or glob == "" then
    return nil
  end
  local i = 1
  local out = { "^" }
  while i <= #glob do
    local ch = glob:sub(i, i)
    local next2 = glob:sub(i, i + 1)
    if next2 == "**" then
      out[#out + 1] = ".*"
      i = i + 2
    elseif ch == "*" then
      out[#out + 1] = "[^/]*"
      i = i + 1
    elseif ch == "?" then
      out[#out + 1] = "."
      i = i + 1
    else
      out[#out + 1] = ch:gsub("([%%%+%-%^%$%(%)%.%[%]%?])", "%%%1")
      i = i + 1
    end
  end
  out[#out + 1] = "$"
  return table.concat(out)
end

local function path_matches_glob(filename, path_glob)
  if not path_glob or path_glob == "" then
    return true
  end
  local pattern = glob_to_pattern(path_glob:gsub("\\", "/"))
  for _, project in ipairs(core.projects) do
    if common.path_belongs_to(filename, project.path) then
      return common.relative_path(project.path, filename):gsub("\\", "/"):match(pattern) ~= nil
    end
  end
  return filename:gsub("\\", "/"):match(pattern) ~= nil
end

function ResultsView:new(path, text, fn, path_glob, search_opts)
  ResultsView.super.new(self)
  self.scrollable = true
  self.brightness = 0
  self.max_h_scroll = 0
  self.display_results = {}
  self:begin_search(path, text, fn, path_glob, search_opts)
end


function ResultsView:get_name()
  if self.path_glob and self.path_glob ~= "" then
    return "Search Results [" .. self.path_glob .. "]"
  end
  return "Search Results"
end


local function find_all_matches_in_file(t, filename, fn)
  local fp = io.open(filename)
  if not fp then return t end
  local n = 1
  for line in fp:lines() do
    local s = fn(line)
    if s then
      -- Insert maximum 256 characters. If we insert more, for compiled files, which can have very long lines
      -- things tend to get sluggish. If our line is longer than 80 characters, begin to truncate the thing.
      local start_index = math.max(s - 80, 1)
      local text = (start_index > 1 and "..." or "") .. line:sub(start_index, 256 + start_index)
      if #line > 256 + start_index then text = text .. "..." end
      table.insert(t, { file = filename, text = text, line = n, col = s })
      core.redraw = true
    end
    if n % 100 == 0 then coroutine.yield(0) end
    n = n + 1
    core.redraw = true
  end
  fp:close()
end


function ResultsView:rebuild_display_results()
  self.display_results = {}
  local last_file
  for i, item in ipairs(self.results) do
    if item.file ~= last_file then
      self.display_results[#self.display_results + 1] = {
        kind = "file",
        file = item.file,
      }
      last_file = item.file
    end
    self.display_results[#self.display_results + 1] = {
      kind = "match",
      result_idx = i,
    }
  end
end


local function collect_roots(path)
  if path then
    local info = system.get_file_info(path)
    if info and info.type == "dir" then
      return { path }
    end
  end
  local roots = {}
  for _, project in ipairs(core.projects) do
    roots[#roots + 1] = project.path
  end
  return roots
end

local function collect_native_files(path, path_glob)
  if not native_project_search then
    return nil
  end
  if path then
    local info = system.get_file_info(path)
    if info and info.type == "file" then
      return { path }
    end
  end
  local roots = collect_roots(path)
  local files = native_project_search.collect_files(roots, {
    show_hidden = false,
    max_size_bytes = config.file_size_limit * 1e6,
    path_glob = path_glob,
    max_files = config.project_scan.max_files,
    exclude_dirs = config.project_scan.exclude_dirs,
  })
  if path and (not system.get_file_info(path) or system.get_file_info(path).type ~= "dir") then
    local filtered = {}
    for _, file in ipairs(files) do
      if file:find(path, 1, true) == 1 then
        filtered[#filtered + 1] = file
      end
    end
    files = filtered
  end
  return files
end

function ResultsView:begin_search(path, text, fn, path_glob, search_opts)
  self.search_args = { path, text, fn, path_glob, search_opts }
  self.results = {}
  self.last_file_idx = 1
  self.query = text
  self.path_glob = path_glob
  self.searching = true
  self.selected_idx = 0
  self.search_opts = search_opts

  if native_project_search and search_opts then
    local files = collect_native_files(path, path_glob) or {}
    self.last_file_idx = #files
    local handle = native_project_search.search({
      files = files,
      query = search_opts.query,
      mode = search_opts.mode,
      no_case = search_opts.no_case,
    })
    core.add_thread(function()
      while true do
        local polled = native_project_search.poll(handle, 128)
        if polled and polled.error then
          core.error("%s", polled.error)
          self.searching = false
          break
        end
        if polled and polled.results then
          for _, item in ipairs(polled.results) do
            self.results[#self.results + 1] = {
              file = item.file,
              text = item.text,
              line = item.line,
              col = item.col,
            }
          end
          self:rebuild_display_results()
          core.redraw = true
        end
        if polled and polled.done then
          self.searching = false
          self.brightness = 100
          self:rebuild_display_results()
          core.redraw = true
          break
        end
        coroutine.yield(0.01)
      end
    end, self.results)
    self.scroll.to.y = 0
    return
  end

  core.add_thread(function()
    local i = 1
    for k, project in ipairs(core.projects) do
      for dir_name, file in project:files() do
        if file.type == "file"
            and (not path or file.filename:find(path, 1, true) == 1)
            and path_matches_glob(file.filename, path_glob) then
          find_all_matches_in_file(self.results, file.filename, fn)
          self:rebuild_display_results()
        end
        self.last_file_idx = i
        i = i + 1
      end
    end
    self.searching = false
    self.brightness = 100
    self:rebuild_display_results()
    core.redraw = true
  end, self.results)

  self.scroll.to.y = 0
end


function ResultsView:refresh()
  self:begin_search(table.unpack(self.search_args))
end


function ResultsView:on_mouse_moved(mx, my, ...)
  ResultsView.super.on_mouse_moved(self, mx, my, ...)
  self.selected_idx = 0
  for i, item, x,y,w,h in self:each_visible_result() do
    if mx >= x and my >= y and mx < x + w and my < y + h then
      if item.kind == "match" then
        self.selected_idx = item.result_idx
      end
      break
    end
  end
end


function ResultsView:on_mouse_pressed(...)
  local caught = ResultsView.super.on_mouse_pressed(self, ...)
  if not caught then
    return self:open_selected_result()
  end
end


function ResultsView:open_selected_result()
  local res = self.results[self.selected_idx]
  if not res then
    return
  end
  core.try(function()
    local dv = core.root_view:open_doc(core.open_doc(res.file))
    core.root_view.root_node:update_layout()
    dv.doc:set_selection(res.line, res.col)
    dv:scroll_to_line(res.line, false, true)
  end)
  return true
end


function ResultsView:update()
  self:move_towards("brightness", 0, 0.1)
  ResultsView.super.update(self)
end


function ResultsView:get_results_yoffset()
  return style.font:get_height() + style.padding.y * 3
end


function ResultsView:get_line_height()
  return style.padding.y + style.font:get_height()
end


function ResultsView:get_scrollable_size()
  return self:get_results_yoffset() + #self.display_results * self:get_line_height()
end


function ResultsView:get_h_scrollable_size()
  return self.max_h_scroll
end


function ResultsView:get_visible_results_range()
  local lh = self:get_line_height()
  local oy = self:get_results_yoffset()
  local min = self.scroll.y+oy-style.font:get_height()
  min = math.max(1, math.floor(min / lh))
  return min, min + math.floor(self.size.y / lh) + 1
end


function ResultsView:each_visible_result()
  return coroutine.wrap(function()
    local lh = self:get_line_height()
    local x, y = self:get_content_offset()
    local min, max = self:get_visible_results_range()
    y = y + self:get_results_yoffset() + lh * (min - 1)
    for i = min, max do
      local item = self.display_results[i]
      if not item then break end
      local _, _, w = self:get_content_bounds()
      coroutine.yield(i, item, x, y, w, lh)
      y = y + lh
    end
  end)
end


function ResultsView:get_selected_display_index()
  if self.selected_idx <= 0 then
    return nil
  end
  for i, item in ipairs(self.display_results) do
    if item.kind == "match" and item.result_idx == self.selected_idx then
      return i
    end
  end
  return nil
end


function ResultsView:scroll_to_make_selected_visible()
  local h = self:get_line_height()
  local idx = self:get_selected_display_index()
  if not idx then
    return
  end
  local y = h * (idx - 1)
  self.scroll.to.y = math.min(self.scroll.to.y, y)
  self.scroll.to.y = math.max(self.scroll.to.y, y + h - self.size.y + self:get_results_yoffset())
end


function ResultsView:draw()
  self:draw_background(style.background)

  -- status
  local ox, oy = self.position.x, self.position.y
  local yoffset = self:get_results_yoffset()
  renderer.draw_rect(self.position.x, self.position.y, self.size.x, yoffset, style.background)
  if self.scroll.y ~= 0 then
    renderer.draw_rect(self.position.x, self.position.y+yoffset, self.size.x, style.divider_size, style.divider)
  end

  local x, y = ox + style.padding.x, oy + style.padding.y
  local text
  if self.searching then
    text = string.format("Searching (%d files, %d matches) for %q%s...",
      self.last_file_idx, #self.results, self.query,
      self.path_glob and self.path_glob ~= "" and (" in " .. self.path_glob) or "")
  else
    text = string.format("Found %d matches for %q%s",
      #self.results, self.query,
      self.path_glob and self.path_glob ~= "" and (" in " .. self.path_glob) or "")
  end
  local color = common.lerp(style.text, style.accent, self.brightness / 100)
  renderer.draw_text(style.font, text, x, y, color)

  -- horizontal line
  local x = ox + style.padding.x
  local w = self.size.x - style.padding.x * 2
  local h = style.divider_size
  local color = common.lerp(style.dim, style.text, self.brightness / 100)
  renderer.draw_rect(x, oy + yoffset - style.padding.y, w, h, color)
  if self.searching then
    renderer.draw_rect(x, oy + yoffset - style.padding.y, w, h, style.text)
  end

  -- results
  local _, _, bw = self:get_content_bounds()
  core.push_clip_rect(ox, oy+yoffset + style.divider_size, bw, self.size.y-yoffset)
  local y1, y2 = self.position.y, self.position.y + self.size.y
  for i, item, x,y,w,h in self:each_visible_result() do
    x = x + style.padding.x
    if item.kind == "file" then
      local label = core.root_project():normalize_path(item.file)
      common.draw_text(style.font, style.accent, label, "left", x, y, w, h)
    else
      local match = self.results[item.result_idx]
      local color = style.text
      if item.result_idx == self.selected_idx then
        color = style.accent
        renderer.draw_rect(x - style.padding.x, y, w, h, style.line_highlight)
      end
      local text = string.format("  line %d (col %d): ", match.line, match.col)
      x = common.draw_text(style.font, style.dim, text, "left", x, y, w, h)
      x = common.draw_text(style.code_font, color, match.text, "left", x, y, w, h)
    end
    self.max_h_scroll = math.max(self.max_h_scroll, x)
  end
  core.pop_clip_rect()

  self:draw_scrollbar()
end

---@param path string
---@param text string
---@param fn fun(line_text:string):...
---@return plugins.projectsearch.resultsview?
local function begin_search(path, text, fn, path_glob, search_opts)
  if text == "" then
    core.error("Expected non-empty string")
    return
  end
  local rv = ResultsView(path, text, fn, path_glob, search_opts)
  core.root_view:get_active_node_default():add_view(rv)
  return rv
end


local function get_selected_text()
  local view = core.active_view
  local doc = (view and view.doc) and view.doc or nil
  if doc then
    return doc:get_text(table.unpack({ doc:get_selection() }))
  end
end

---@class plugins.projectsearch
local projectsearch = {}

---@type plugins.projectsearch.resultsview
projectsearch.ResultsView = ResultsView

---@param text string
---@param path string
---@param insensitive? boolean
---@return plugins.projectsearch.resultsview?
function projectsearch.search_plain(text, path, insensitive)
  return begin_search(path, text, function(line_text)
    if insensitive then
      return line_text:lower():find(text:lower(), nil, true)
    else
      return line_text:find(text, nil, true)
    end
  end, projectsearch.pending_path_glob, {
    query = text,
    mode = "plain",
    no_case = insensitive and true or false,
  })
end

---@param text string
---@param path string
---@param insensitive? boolean
---@return plugins.projectsearch.resultsview?
function projectsearch.search_regex(text, path, insensitive)
  local re, errmsg
  if insensitive then
    re, errmsg = regex.compile(text, "i")
  else
    re, errmsg = regex.compile(text)
  end
  if not re then core.log("%s", errmsg) return end
  return begin_search(path, text, function(line_text)
    return regex.cmatch(re, line_text)
  end, projectsearch.pending_path_glob, {
    query = text,
    mode = "regex",
    no_case = insensitive and true or false,
  })
end

---@param text string
---@param path string
---@param insensitive? boolean
---@return plugins.projectsearch.resultsview?
function projectsearch.search_fuzzy(text, path, insensitive)
  return begin_search(path, text, function(line_text)
    if insensitive then
      return common.fuzzy_match(line_text:lower(), text:lower()) and 1
    else
      return common.fuzzy_match(line_text, text) and 1
    end
  end, projectsearch.pending_path_glob, {
    query = text,
    mode = "fuzzy",
    no_case = insensitive and true or false,
  })
end


local function prompt_path_glob(label, submit)
  core.command_view:enter(label, {
    submit = function(text)
      submit(text ~= "" and text or nil)
    end
  })
end


local function enter_search(kind, path, submit, opts)
  opts = opts or {}
  core.command_view:enter(kind .. " In " .. (path or "Project"), {
    text = opts.text,
    select_text = opts.select_text,
    submit = function(text)
      prompt_path_glob("Path Glob Filter (optional)", function(path_glob)
        projectsearch.pending_path_glob = path_glob
        submit(text)
        projectsearch.pending_path_glob = nil
      end)
    end
  })
end


command.add(nil, {
  ["project-search:find"] = function(path)
    enter_search("Find Text", path, function(text)
      projectsearch.search_plain(text, path, true)
    end, {
      text = get_selected_text(),
      select_text = true,
    })
  end,

  ["project-search:find-regex"] = function(path)
    enter_search("Find Regex", path, function(text)
      projectsearch.search_regex(text, path, true)
    end)
  end,

  ["project-search:fuzzy-find"] = function(path)
    enter_search("Fuzzy Find Text", path, function(text)
      projectsearch.search_fuzzy(text, path, true)
    end, {
      text = get_selected_text(),
      select_text = true,
    })
  end,
})


command.add(ResultsView, {
  ["project-search:select-previous"] = function()
    local view = core.active_view
    view.selected_idx = math.max(view.selected_idx - 1, 1)
    view:scroll_to_make_selected_visible()
  end,

  ["project-search:select-next"] = function()
    local view = core.active_view
    view.selected_idx = math.min(view.selected_idx + 1, #view.results)
    view:scroll_to_make_selected_visible()
  end,

  ["project-search:open-selected"] = function()
    core.active_view:open_selected_result()
  end,

  ["project-search:refresh"] = function()
    core.active_view:refresh()
  end,

  ["project-search:move-to-previous-page"] = function()
    local view = core.active_view
    view.scroll.to.y = view.scroll.to.y - view.size.y
  end,

  ["project-search:move-to-next-page"] = function()
    local view = core.active_view
    view.scroll.to.y = view.scroll.to.y + view.size.y
  end,

  ["project-search:move-to-start-of-doc"] = function()
    local view = core.active_view
    view.scroll.to.y = 0
  end,

  ["project-search:move-to-end-of-doc"] = function()
    local view = core.active_view
    view.scroll.to.y = view:get_scrollable_size()
  end
})

keymap.add {
  ["f5"]                 = "project-search:refresh",
  ["ctrl+shift+f"]       = "project-search:find",
  ["up"]                 = "project-search:select-previous",
  ["down"]               = "project-search:select-next",
  ["return"]             = "project-search:open-selected",
  ["pageup"]             = "project-search:move-to-previous-page",
  ["pagedown"]           = "project-search:move-to-next-page",
  ["ctrl+home"]          = "project-search:move-to-start-of-doc",
  ["ctrl+end"]           = "project-search:move-to-end-of-doc",
  ["home"]               = "project-search:move-to-start-of-doc",
  ["end"]                = "project-search:move-to-end-of-doc"
}


return projectsearch
