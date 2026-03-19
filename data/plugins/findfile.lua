-- mod-version:4

local core = require "core"
local command = require "core.command"
local common = require "core.common"
local config = require "core.config"
local keymap = require "core.keymap"
local native_project_model = require "project_model"
local native_picker = require "picker"

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
    if #core.projects == 0 then
      command.perform("core:open-file")
      return
    end
    local files, complete = {}, false
    local file_limit = config.plugins.findfile.file_limit

    local refresh = coroutine.wrap(function()
      local roots = {}
      for i, project in ipairs(core.projects) do
        roots[i] = project.path
      end
      local cached = native_project_model.get_all_files(roots, {
        max_size_bytes = config.file_size_limit * 1e6,
        max_files = file_limit,
        exclude_dirs = config.project_scan.exclude_dirs,
      })
      local n = 0
      for _, filename in ipairs(cached) do
        if complete or #files >= file_limit then break end
        for i, project in ipairs(core.projects) do
          if common.path_belongs_to(filename, project.path) then
            local info = { type = "file", size = 0, filename = filename }
            if not project:is_ignored(info, filename) then
              files[#files + 1] = i == 1 and filename:sub(#project.path + 2)
                or common.home_encode(filename)
            end
            break
          end
        end
        n = n + 1
        if n % 200 == 0 then
          core.command_view:update_suggestions()
          coroutine.yield(0)
        end
      end
    end)

    local wait = refresh()
    if wait ~= nil then
      core.add_thread(function()
        while wait ~= nil do
          wait = refresh()
          coroutine.yield(wait or 0)
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
        original_files = native_picker.rank_strings(files, text, true, text == "" and core.visited_files or nil)
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
