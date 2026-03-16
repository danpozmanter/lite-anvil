-- mod-version:4

local core = require "core"
local command = require "core.command"
local common = require "core.common"
local config = require "core.config"
local keymap = require "core.keymap"
local native_manifest = nil
local native_project_model = nil
local native_picker = nil

do
  local ok, mod = pcall(require, "project_manifest")
  if ok then
    native_manifest = mod
  end
  ok, mod = pcall(require, "project_model")
  if ok then
    native_project_model = mod
  end
  ok, mod = pcall(require, "picker")
  if ok then
    native_picker = mod
  end
end

config.plugins.findfile = common.merge({
  -- how many files from the project we store in a list before we stop
  file_limit = 20000,
  -- the maximum amount of time we spend gathering files before stopping
  max_search_time = 10.0,
  -- the amount of time we wait between loops of gathering files
  interval = 0,
  -- the amount of time we spend in a single loop (by default, half a frame)
  max_loop_time = 0.5 / config.fps
}, config.plugins.findfile)


command.add(nil, {
  ["core:find-file"] = function()
    local files, complete = {}, false
    if native_project_model then
      local roots = {}
      for i, project in ipairs(core.projects) do
        roots[i] = project.path
      end
      local cached = native_project_model.get_all_files(roots, {
        max_size_bytes = config.file_size_limit * 1e6,
        max_files = config.plugins.findfile.file_limit,
      })
      for _, filename in ipairs(cached) do
        if #files > config.plugins.findfile.file_limit then
          break
        end
        for i, project in ipairs(core.projects) do
          if common.path_belongs_to(filename, project.path) then
            local info = project:get_file_info(filename)
            if info and info.type == "file" then
              files[#files + 1] = i == 1 and filename:sub(#project.path + 2) or common.home_encode(filename)
            end
            break
          end
        end
      end
    elseif native_manifest then
      for i, project in ipairs(core.projects) do
        local cached = native_manifest.get_files(project.path, {
          max_size_bytes = config.file_size_limit * 1e6
        })
        for _, filename in ipairs(cached) do
          if #files > config.plugins.findfile.file_limit then
            break
          end
          local info = project:get_file_info(filename)
          if info and info.type == "file" then
            files[#files + 1] = i == 1 and filename:sub(#project.path + 2) or common.home_encode(filename)
          end
        end
      end
    end
    local refresh = coroutine.wrap(function()
      if native_manifest then
        return
      end
      local start, total = system.get_time(), 0
      for i, project in ipairs(core.projects) do
        for project, item in project:files() do
          if complete then return end
          if #files > config.plugins.findfile.file_limit then 
            core.command_view:update_suggestions() 
            return 
          end
          table.insert(files, i == 1 and item.filename:sub(#project.path + 2) or common.home_encode(item.filename))
          local diff = system.get_time() - start
          if diff > config.plugins.findfile.max_loop_time then
            core.command_view:update_suggestions()
            total = total + diff
            if total > config.plugins.findfile.max_search_time then return end
            coroutine.yield(config.plugins.findfile.interval)
            start = system.get_time()
          end
        end
      end
    end)

    local wait = refresh()
    if wait then
      core.add_thread(function()
        while wait do
          wait = refresh()
          coroutine.yield(wait)
        end
      end)
    end
    local original_files
    core.command_view:enter("Open File From Project", {
      submit = function(text, item)
        text = item and item.text or text
        core.root_view:open_doc(core.open_doc(common.home_expand(text)))
        complete = true
      end,
      suggest = function(text)
        if original_files and text == "" then 
          return original_files
        end
        if native_picker then
          original_files = native_picker.rank_strings(files, text, true, text == "" and core.visited_files or nil)
        else
          original_files = common.fuzzy_match_with_recents(files, core.visited_files, text)
        end
        return original_files
      end,
      cancel = function()
        complete = true
      end
    })
  end
})

keymap.add({
  [PLATFORM == "Mac OS X" and "cmd+shift+o" or "ctrl+shift+o"] = "core:find-file"
})
