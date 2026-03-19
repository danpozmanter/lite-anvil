local core = require "core"
local common = require "core.common"
local config = require "core.config"
local native_project_fs = require "project_fs"
local dirwatch = {}

function dirwatch:__index(idx)
  local value = rawget(self, idx)
  if value ~= nil then return value end
  return dirwatch[idx]
end

---A directory watcher.
---
---It can be used to watch changes in files and directories.
---The user repeatedly calls dirwatch:check() with a callback inside a coroutine.
---If a file or directory had changed, the callback is called with the corresponding file.
---@class core.dirwatch
---@field scanned { [string]: number } Stores the last modified time of paths.
---@field watched { [string]: boolean|number } Stores the paths that are being watched, and their unique fd.
---@field reverse_watched { [number]: string } Stores the paths mapped by their unique fd.
---@field monitor dirmonitor The dirmonitor instance associated with this watcher.
---@field single_watch_top string The first file that is being watched.
---@field single_watch_count number Number of files that are being watched.

---Creates a directory monitor.
---@return core.dirwatch
function dirwatch.new()
  local t = {
    scanned = {},
    watched = {},
    reverse_watched = {},
    native_watches = {},
    monitor = dirmonitor.new(),
    single_watch_top = nil,
    single_watch_count = 0
  }
  setmetatable(t, dirwatch)
  return t
end


---Schedules a path for scanning.
---If a path points to a file, it is watched directly.
---Otherwise, the contents of the path are watched (non-recursively).
---@param path string
---@param  unwatch? boolean If true, remove this directory from the watch list.
function dirwatch:scan(path, unwatch)
  if unwatch == false then return self:unwatch(path) end
  return self:watch(path)
end


---Watches a path.
---
---It is recommended to call this function on every subdirectory if the given path
---points to a directory. This is not required for Windows, but should be done to ensure
---cross-platform compatibility.
---
---Using this function on individual files is possible, but discouraged as it can cause
---system resource exhaustion.
---@param path string The path to watch. This should be an absolute path.
---@param unwatch? boolean If true, the path is removed from the watch list.
function dirwatch:watch(path, unwatch)
  if unwatch == false then return self:unwatch(path) end
  local info = system.get_file_info(path)
  if not info then return end
  if not self.native_watches[path] then
    local ok, watch_id = pcall(native_project_fs.watch_project, path)
    if ok and watch_id then
      self.native_watches[path] = { id = watch_id, type = info.type }
    else
      self.scanned[path] = info.modified
    end
  end
end

---Removes a path from the watch list.
---@param directory string The path to remove. This should be an absolute path.
function dirwatch:unwatch(directory)
  if self.native_watches[directory] then
    native_project_fs.cancel_watch(self.native_watches[directory].id)
    self.native_watches[directory] = nil
    return
  end
  if self.scanned[directory] then
    self.scanned[directory] = nil
  end
end

---Checks each watched paths for changes.
---This function must be called in a coroutine, e.g. inside a thread created with `core.add_thread()`.
---@param change_callback fun(path: string)
---@param scan_time? number Maximum amount of time, in seconds, before the function yields execution.
---@param wait_time? number The duration to yield execution (in seconds).
---@return boolean # If true, a path had changed.
function dirwatch:check(change_callback, scan_time, wait_time)
  local had_change = false
  local delivered = {}
  for path, watch in pairs(self.native_watches) do
    local ok, changes = pcall(native_project_fs.poll_changes, watch.id)
    if ok and changes then
      for _, changed in ipairs(changes) do
        local target = watch.type == "dir" and path or path
        if watch.type == "file" then
          if changed == path and not delivered[target] then
            delivered[target] = true
            change_callback(target)
            had_change = true
          end
        elseif not delivered[target] then
          delivered[target] = true
          change_callback(target)
          had_change = true
        end
      end
    end
  end
  local start_time = system.get_time()
  for directory, old_modified in pairs(self.scanned) do
    if old_modified then
      local info = system.get_file_info(directory)
      local new_modified = info and info.modified
      if old_modified ~= new_modified then
        change_callback(directory)
        had_change = true
        self.scanned[directory] = new_modified
      end
    end
    if system.get_time() - start_time > (scan_time or 0.01) then
      coroutine.yield(wait_time or 0.01)
      start_time = system.get_time()
    end
  end
  return had_change
end

return dirwatch
