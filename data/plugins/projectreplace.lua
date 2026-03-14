-- mod-version:4
local core    = require "core"
local common  = require "core.common"
local keymap  = require "core.keymap"
local command = require "core.command"
local config  = require "core.config"
local style   = require "core.style"
local View    = require "core.view"
local native_project_search = nil

do
  local ok, mod = pcall(require, "project_search")
  if ok then native_project_search = mod end
end

config.plugins.projectreplace = common.merge({
  backup_originals = true,
}, config.plugins.projectreplace)

local ReplaceView = View:extend()

function ReplaceView:__tostring() return "ReplaceView" end

ReplaceView.context = "session"

function ReplaceView:new(path, search, replace, fn_find, fn_apply, path_glob, native_search_opts, native_replace_opts)
  ReplaceView.super.new(self)
  self.scrollable    = true
  self.max_h_scroll  = 0
  self.path          = path
  self.search        = search
  self.replace       = replace
  self.path_glob     = path_glob
  self.fn_find       = fn_find
  self.fn_apply      = fn_apply
  self.results       = {}
  self.phase         = "scanning"
  self.last_file_idx = 1
  self.selected_idx  = 0
  self.brightness    = 0
  self.replaced_count = 0
  self.replaced_files = 0
  self.operation      = "replace"
  self.native_search_opts = native_search_opts
  self.native_replace_opts = native_replace_opts
  self:begin_scan()
end

function ReplaceView:get_name()
  return "Replace Results"
end

local function collect_matches(results, filename, fn_find)
  local fp = io.open(filename)
  if not fp then return end
  local n = 1
  for line in fp:lines() do
    local s = fn_find(line)
    if s then
      local start_index = math.max(s - 80, 1)
      local text = (start_index > 1 and "..." or "") .. line:sub(start_index, 256 + start_index)
      if #line > 256 + start_index then text = text .. "..." end
      table.insert(results, { file = filename, text = text, line = n, col = s })
      core.redraw = true
    end
    if n % 100 == 0 then coroutine.yield(0) end
    n = n + 1
  end
  fp:close()
end

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
  if not pattern then
    return true
  end
  for _, project in ipairs(core.projects) do
    if common.path_belongs_to(filename, project.path) then
      return common.relative_path(project.path, filename):gsub("\\", "/"):match(pattern) ~= nil
    end
  end
  return filename:gsub("\\", "/"):match(pattern) ~= nil
end

function ReplaceView:begin_scan()
  self.results       = {}
  self.phase         = "scanning"
  self.last_file_idx = 1

  if native_project_search and self.native_search_opts then
    local handle = native_project_search.search(self.native_search_opts)
    self.last_file_idx = #(self.native_search_opts.files or {})
    core.add_thread(function()
      while true do
        local polled = native_project_search.poll(handle, 128)
        if polled and polled.error then
          core.error("%s", polled.error)
          self.phase = "confirming"
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
          core.redraw = true
        end
        if polled and polled.done then
          self.phase = "confirming"
          self.brightness = 100
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
    for _, project in ipairs(core.projects) do
      for _, file in project:files() do
        if file.type == "file"
            and (not self.path or file.filename:find(self.path, 1, true) == 1)
            and path_matches_glob(file.filename, self.path_glob) then
          collect_matches(self.results, file.filename, self.fn_find)
        end
        self.last_file_idx = i
        i = i + 1
      end
    end
    self.phase      = "confirming"
    self.brightness = 100
    core.redraw = true
  end, self.results)

  self.scroll.to.y = 0
end

function ReplaceView:apply_replace()
  self.phase          = "replacing"
  self.replaced_count = 0
  self.replaced_files = 0
  core.redraw = true

  core.add_thread(function()
    if native_project_search and self.native_replace_opts then
      local stats = native_project_search.replace(self.native_replace_opts)
      self.replaced_count = stats.replaced_count or 0
      self.replaced_files = stats.replaced_files or 0
      self.phase = "done"
      self.brightness = 100
      core.redraw = true
      return
    end

    local files = {}
    local seen  = {}
    for _, item in ipairs(self.results) do
      if not seen[item.file] then
        seen[item.file] = true
        table.insert(files, item.file)
      end
    end

    for _, filename in ipairs(files) do
      local fp = io.open(filename, "rb")
      if fp then
        local content = fp:read("*a")
        fp:close()
        local new_content, count = self.fn_apply(content)
        if count > 0 then
          if config.plugins.projectreplace.backup_originals then
            local backup = io.open(filename .. ".bak", "wb")
            if backup then
              backup:write(content)
              backup:close()
            end
          end
          local out = io.open(filename, "wb")
          if out then
            out:write(new_content)
            out:close()
            self.replaced_count = self.replaced_count + count
            self.replaced_files = self.replaced_files + 1
          end
        end
      end
      coroutine.yield(0)
      core.redraw = true
    end

    self.phase      = "done"
    self.brightness = 100
    core.redraw = true
  end)
end

function ReplaceView:on_mouse_moved(mx, my, ...)
  ReplaceView.super.on_mouse_moved(self, mx, my, ...)
  self.selected_idx = 0
  for i, item, x, y, w, h in self:each_visible_result() do
    if mx >= x and my >= y and mx < x + w and my < y + h then
      self.selected_idx = i
      break
    end
  end
end

function ReplaceView:on_mouse_pressed(...)
  local caught = ReplaceView.super.on_mouse_pressed(self, ...)
  if not caught then
    return self:open_selected_result()
  end
end

function ReplaceView:open_selected_result()
  local res = self.results[self.selected_idx]
  if not res then return end
  core.try(function()
    local dv = core.root_view:open_doc(core.open_doc(res.file))
    core.root_view.root_node:update_layout()
    dv.doc:set_selection(res.line, res.col)
    dv:scroll_to_line(res.line, false, true)
  end)
  return true
end

function ReplaceView:update()
  self:move_towards("brightness", 0, 0.1)
  ReplaceView.super.update(self)
end

function ReplaceView:get_results_yoffset()
  return style.font:get_height() + style.padding.y * 3
end

function ReplaceView:get_line_height()
  return style.padding.y + style.font:get_height()
end

function ReplaceView:get_scrollable_size()
  return self:get_results_yoffset() + #self.results * self:get_line_height()
end

function ReplaceView:get_h_scrollable_size()
  return self.max_h_scroll
end

function ReplaceView:get_visible_results_range()
  local lh  = self:get_line_height()
  local oy  = self:get_results_yoffset()
  local min = math.max(1, math.floor((self.scroll.y + oy - style.font:get_height()) / lh))
  return min, min + math.floor(self.size.y / lh) + 1
end

function ReplaceView:each_visible_result()
  return coroutine.wrap(function()
    local lh    = self:get_line_height()
    local x, y  = self:get_content_offset()
    local min, max = self:get_visible_results_range()
    y = y + self:get_results_yoffset() + lh * (min - 1)
    for i = min, max do
      local item = self.results[i]
      if not item then break end
      local _, _, w = self:get_content_bounds()
      coroutine.yield(i, item, x, y, w, lh)
      y = y + lh
    end
  end)
end

function ReplaceView:scroll_to_make_selected_visible()
  local h = self:get_line_height()
  local y = h * (self.selected_idx - 1)
  self.scroll.to.y = math.min(self.scroll.to.y, y)
  self.scroll.to.y = math.max(self.scroll.to.y, y + h - self.size.y + self:get_results_yoffset())
end

function ReplaceView:draw()
  self:draw_background(style.background)

  local ox, oy   = self.position.x, self.position.y
  local yoffset  = self:get_results_yoffset()
  renderer.draw_rect(ox, oy, self.size.x, yoffset, style.background)
  if self.scroll.y ~= 0 then
    renderer.draw_rect(ox, oy + yoffset, self.size.x, style.divider_size, style.divider)
  end

  local color = common.lerp(style.text, style.accent, self.brightness / 100)
  local x, y  = ox + style.padding.x, oy + style.padding.y

  local msg
  if self.phase == "scanning" then
    if self.operation == "swap" then
      msg = string.format("Scanning (%d files, %d matches) to swap %q and %q%s...",
        self.last_file_idx, #self.results, self.search, self.replace,
        self.path_glob and self.path_glob ~= "" and (" in " .. self.path_glob) or "")
    else
      msg = string.format("Searching (%d files, %d matches) for %q%s...",
        self.last_file_idx, #self.results, self.search,
        self.path_glob and self.path_glob ~= "" and (" in " .. self.path_glob) or "")
    end
  elseif self.phase == "confirming" then
    if self.operation == "swap" then
      msg = string.format("Found %d matches to swap %q and %q%s — press F5 to apply",
        #self.results, self.search, self.replace,
        self.path_glob and self.path_glob ~= "" and (" in " .. self.path_glob) or "")
    else
      msg = string.format("Found %d matches for %q%s — press F5 to replace all with %q",
        #self.results, self.search,
        self.path_glob and self.path_glob ~= "" and (" in " .. self.path_glob) or "",
        self.replace)
    end
  elseif self.phase == "replacing" then
    if self.operation == "swap" then
      msg = string.format("Swapping... (%d files written)", self.replaced_files)
    else
      msg = string.format("Replacing... (%d files written)", self.replaced_files)
    end
  else
    if self.operation == "swap" then
      msg = string.format("Done — swapped %d occurrences in %d files (%q <-> %q)",
        self.replaced_count, self.replaced_files, self.search, self.replace)
    else
      msg = string.format("Done — replaced %d occurrences in %d files (%q -> %q)",
        self.replaced_count, self.replaced_files, self.search, self.replace)
    end
  end
  renderer.draw_text(style.font, msg, x, y, color)

  local dcolor = common.lerp(style.dim, style.text, self.brightness / 100)
  renderer.draw_rect(x, oy + yoffset - style.padding.y,
    self.size.x - style.padding.x * 2, style.divider_size, dcolor)

  local _, _, bw = self:get_content_bounds()
  core.push_clip_rect(ox, oy + yoffset + style.divider_size, bw, self.size.y - yoffset)
  for i, item, ix, iy, iw, ih in self:each_visible_result() do
    local tc = style.text
    if i == self.selected_idx then
      tc = style.accent
      renderer.draw_rect(ix, iy, iw, ih, style.line_highlight)
    end
    ix = ix + style.padding.x
    local label = string.format("%s at line %d (col %d): ",
      core.root_project():normalize_path(item.file), item.line, item.col)
    ix = common.draw_text(style.font, style.dim, label, "left", ix, iy, iw, ih)
    ix = common.draw_text(style.code_font, tc, item.text, "left", ix, iy, iw, ih)
    self.max_h_scroll = math.max(self.max_h_scroll, ix)
  end
  core.pop_clip_rect()

  self:draw_scrollbar()
end


local function plain_replace(content, search_text, replace_text)
  local parts = {}
  local count  = 0
  local pos    = 1
  while true do
    local s, e = content:find(search_text, pos, true)
    if not s then
      table.insert(parts, content:sub(pos))
      break
    end
    table.insert(parts, content:sub(pos, s - 1))
    table.insert(parts, replace_text)
    pos   = e + 1
    count = count + 1
  end
  return table.concat(parts), count
end

local function make_plain_matcher(search_text, case_sensitive)
  local needle = case_sensitive and search_text or search_text:lower()
  return {
    find = function(text, pos)
      local haystack = case_sensitive and text or text:lower()
      local s, e = haystack:find(needle, pos or 1, true)
      if not s then
        return nil
      end
      return s, e + 1
    end
  }
end

local function make_regex_matcher(search_text, case_sensitive)
  local re, errmsg = regex.compile(search_text, case_sensitive and "" or "i")
  if not re then
    return nil, errmsg
  end
  return {
    find = function(text, pos)
      return re:cmatch(text, pos or 1)
    end
  }
end

local function replace_with_matcher(content, matcher, replace_text)
  local parts = {}
  local count = 0
  local pos = 1
  while pos <= #content + 1 do
    local s, e = matcher.find(content, pos)
    if not s then
      table.insert(parts, content:sub(pos))
      break
    end
    table.insert(parts, content:sub(pos, s - 1))
    table.insert(parts, replace_text)
    count = count + 1
    if e > s then
      pos = e
    else
      table.insert(parts, content:sub(s, s))
      pos = s + 1
    end
  end
  return table.concat(parts), count
end

local function regex_replace(content, re, replace_text)
  local parts = {}
  local count  = 0
  local pos    = 1
  while pos <= #content do
    local s, e = re:cmatch(content, pos)
    if not s then
      table.insert(parts, content:sub(pos))
      break
    end
    table.insert(parts, content:sub(pos, s - 1))
    table.insert(parts, replace_text)
    count = count + 1
    if e > s then
      pos = e
    else
      table.insert(parts, content:sub(s, s))
      pos = s + 1
    end
  end
  return table.concat(parts), count
end

local function generate_swap_placeholder(content)
  local counter = 0
  while true do
    counter = counter + 1
    local token = ("__LITE_ANVIL_SWAP_%08x_%08x_%08x__"):format(
      math.random(0, 0xffffffff),
      math.random(0, 0xffffffff),
      counter
    )
    if not content:find(token, 1, true) then
      return token
    end
  end
end

local function swap_replace(content, matcher_a, matcher_b, text_a, text_b)
  local placeholder = generate_swap_placeholder(content)
  local after_a, count_a = replace_with_matcher(content, matcher_a, placeholder)

  local parts = {}
  local count_b = 0
  local pos = 1
  while true do
    local s, e = after_a:find(placeholder, pos, true)
    local segment
    if not s then
      segment = after_a:sub(pos)
      local new_segment, seg_count = replace_with_matcher(segment, matcher_b, text_a)
      count_b = count_b + seg_count
      table.insert(parts, new_segment)
      break
    end
    segment = after_a:sub(pos, s - 1)
    local new_segment, seg_count = replace_with_matcher(segment, matcher_b, text_a)
    count_b = count_b + seg_count
    table.insert(parts, new_segment)
    table.insert(parts, placeholder)
    pos = e + 1
  end

  local final_content = table.concat(parts):gsub(placeholder, text_b)
  return final_content, count_a + count_b
end

local function get_selected_text()
  local view = core.active_view
  local doc  = view and view.doc
  if doc then
    return doc:get_text(table.unpack({ doc:get_selection() }))
  end
end

local function open_replace_view(path, search, replace, fn_find, fn_apply, path_glob, operation, native_search_opts, native_replace_opts)
  if search == "" then
    core.error("Expected non-empty search string")
    return
  end
  local rv = ReplaceView(path, search, replace, fn_find, fn_apply, path_glob, native_search_opts, native_replace_opts)
  rv.operation = operation or "replace"
  core.root_view:get_active_node_default():add_view(rv)
  return rv
end

local function collect_native_files(path, path_glob)
  if not native_project_search then
    return nil
  end
  local roots = {}
  if path then
    local info = system.get_file_info(path)
    if info and info.type == "dir" then
      roots[1] = path
    end
  end
  if #roots == 0 then
    for _, project in ipairs(core.projects) do
      roots[#roots + 1] = project.path
    end
  end
  local files = native_project_search.collect_files(roots, {
    show_hidden = false,
    max_size_bytes = config.file_size_limit * 1e6,
    path_glob = path_glob,
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

local function prompt_path_glob(submit)
  core.command_view:enter("Path Glob Filter (optional)", {
    submit = function(text)
      submit(text ~= "" and text or nil)
    end
  })
end

local function parse_yes_no(text, default)
  local trimmed = text:match("^%s*(.-)%s*$"):lower()
  if trimmed == "" then
    return default
  end
  if trimmed == "y" or trimmed == "yes" or trimmed == "true" or trimmed == "1" then
    return true
  end
  if trimmed == "n" or trimmed == "no" or trimmed == "false" or trimmed == "0" then
    return false
  end
  return nil
end

local function prompt_yes_no(label, default, submit)
  core.command_view:enter(label, {
    text = "",
    validate = function(text)
      return parse_yes_no(text, default) ~= nil
    end,
    submit = function(text)
      submit(parse_yes_no(text, default))
    end
  })
end

local function find_first_of(line_text, matcher_a, matcher_b)
  local a = { matcher_a.find(line_text, 1) }
  local b = { matcher_b.find(line_text, 1) }
  local sa = a[1]
  local sb = b[1]
  if sa and sb then
    return math.min(sa, sb)
  end
  return sa or sb
end

local function prompt_swap_options(path, text_a, text_b, submit)
  prompt_yes_no("Regex for A? [y/N]", false, function(a_regex)
    prompt_yes_no("Match Case for A? [Y/n]", true, function(a_case)
      prompt_yes_no("Regex for B? [y/N]", false, function(b_regex)
        prompt_yes_no("Match Case for B? [Y/n]", true, function(b_case)
          prompt_path_glob(function(path_glob)
            submit({
              path = path,
              text_a = text_a,
              text_b = text_b,
              a_regex = a_regex,
              a_case = a_case,
              b_regex = b_regex,
              b_case = b_case,
              path_glob = path_glob,
            })
          end)
        end)
      end)
    end)
  end)
end


command.add(nil, {
  ["project-search:replace"] = function(path)
    core.command_view:enter("Replace Text In " .. (path or "Project"), {
      text = get_selected_text(),
      select_text = true,
      submit = function(search)
        prompt_path_glob(function(path_glob)
          core.command_view:enter("Replace With", {
            submit = function(replace)
              open_replace_view(path, search, replace,
                function(line_text)
                  return line_text:find(search, nil, true)
                end,
                function(content)
                  return plain_replace(content, search, replace)
                end,
                path_glob,
                "replace",
                native_project_search and {
                  files = collect_native_files(path, path_glob) or {},
                  query = search,
                  mode = "plain",
                  no_case = false,
                } or nil,
                native_project_search and {
                  files = collect_native_files(path, path_glob) or {},
                  mode = "plain",
                  query = search,
                  replace = replace,
                  no_case = false,
                  backup_originals = config.plugins.projectreplace.backup_originals ~= false,
                } or nil
              )
            end
          })
        end)
      end
    })
  end,

  ["project-search:replace-regex"] = function(path)
    core.command_view:enter("Replace Regex In " .. (path or "Project"), {
      submit = function(search)
        local re, errmsg = regex.compile(search)
        if not re then core.log("%s", errmsg) return end
        prompt_path_glob(function(path_glob)
          core.command_view:enter("Replace With", {
            submit = function(replace)
              open_replace_view(path, search, replace,
                function(line_text)
                  return regex.cmatch(re, line_text)
                end,
                function(content)
                  return regex_replace(content, re, replace)
                end,
                path_glob,
                "replace",
                native_project_search and {
                  files = collect_native_files(path, path_glob) or {},
                  query = search,
                  mode = "regex",
                  no_case = false,
                } or nil,
                native_project_search and {
                  files = collect_native_files(path, path_glob) or {},
                  mode = "regex",
                  query = search,
                  replace = replace,
                  no_case = false,
                  backup_originals = config.plugins.projectreplace.backup_originals ~= false,
                } or nil
              )
            end
          })
        end)
      end
    })
  end,

  ["project-search:swap"] = function(path)
    core.command_view:enter("Swap Text A In " .. (path or "Project"), {
      text = get_selected_text(),
      select_text = true,
      submit = function(text_a)
        core.command_view:enter("Swap Text B", {
          submit = function(text_b)
            if text_a == "" or text_b == "" then
              core.error("Swap text cannot be empty")
              return
            end
            prompt_swap_options(path, text_a, text_b, function(opts)
              local matcher_a, err_a
              if opts.a_regex then
                matcher_a, err_a = make_regex_matcher(opts.text_a, opts.a_case)
              else
                matcher_a = make_plain_matcher(opts.text_a, opts.a_case)
              end
              if not matcher_a then
                core.error("%s", err_a)
                return
              end
              local matcher_b, err_b
              if opts.b_regex then
                matcher_b, err_b = make_regex_matcher(opts.text_b, opts.b_case)
              else
                matcher_b = make_plain_matcher(opts.text_b, opts.b_case)
              end
              if not matcher_b then
                core.error("%s", err_b)
                return
              end

              open_replace_view(
                opts.path,
                opts.text_a,
                opts.text_b,
                function(line_text)
                  return find_first_of(line_text, matcher_a, matcher_b)
                end,
                function(content)
                  return swap_replace(content, matcher_a, matcher_b, opts.text_a, opts.text_b)
                end,
                opts.path_glob,
                "swap",
                nil,
                native_project_search and {
                  files = collect_native_files(opts.path, opts.path_glob) or {},
                  mode = "swap",
                  query = opts.text_a,
                  replace = opts.text_b,
                  no_case = not opts.a_case,
                  backup_originals = config.plugins.projectreplace.backup_originals ~= false,
                  query_b = opts.text_b,
                  query_b_regex = opts.b_regex,
                  query_b_case = opts.b_case,
                  query_a_regex = opts.a_regex,
                } or nil
              )
            end)
          end
        })
      end
    })
  end,
})


command.add(ReplaceView, {
  ["project-search:confirm-replace"] = function()
    local view = core.active_view
    if view.phase == "confirming" then
      view:apply_replace()
    end
  end,

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
  end,
})

keymap.add {
  ["ctrl+shift+h"] = "project-search:replace",
  ["f5"]           = "project-search:confirm-replace",
  ["up"]           = "project-search:select-previous",
  ["down"]         = "project-search:select-next",
  ["return"]       = "project-search:open-selected",
  ["pageup"]       = "project-search:move-to-previous-page",
  ["pagedown"]     = "project-search:move-to-next-page",
  ["ctrl+home"]    = "project-search:move-to-start-of-doc",
  ["ctrl+end"]     = "project-search:move-to-end-of-doc",
  ["home"]         = "project-search:move-to-start-of-doc",
  ["end"]          = "project-search:move-to-end-of-doc",
}
