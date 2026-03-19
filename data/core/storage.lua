local core = require "core"
local common = require "core.common"
local native_storage = require "storage_native"

local function module_key_to_path(module, key)
  return USERDIR .. PATHSEP .. "storage" .. (module and (PATHSEP .. module .. (key and (PATHSEP .. key:gsub("[\\/]", "-")) or "")) or "")
end


---Provides persistent storage between restarts of the application.
---@class storage
local storage = {}


---Loads data from a persistent storage file.
---
---@param module string The module under which the data is stored.
---@param key string The key under which the data is stored.
---@return string|table|number? data The stored data present for this module, at this key.
function storage.load(module, key)
  local ok, text = pcall(native_storage.load_text, module, key)
  if not ok then
    core.error("error loading storage file for %s[%s]: %s", module, key, text)
    return nil
  end
  if text ~= nil then
    local chunk = text
    if not tostring(text):match("^%s*return[%s%(\"'{%[%-%d]") then
      chunk = "return " .. text
    end
    local func, err = load(chunk, "@storage[" .. module .. ":" .. key .. "]")
    if func then
      return func()
    end
    core.error("error decoding storage file for %s[%s]: %s", module, key, err)
  end
  return nil
end


---Saves data to a persistent storage file.
---
---@param module string The module under which the data is stored.
---@param key string The key under which the data is stored.
---@param value table|string|number The value to store.
function storage.save(module, key, value)
  local ok, err = pcall(native_storage.save_text, module, key, common.serialize(value))
  if not ok then
    core.error("error opening storage file %s for writing: %s", module_key_to_path(module, key), err)
  end
end


---Gets the list of keys saved under a module.
---
---@param module string The module under which the data is stored.
---@return table A table of keys under which data is stored for this module.
function storage.keys(module)
  local ok, keys = pcall(native_storage.keys, module)
  return ok and keys or {}
end


---Clears data for a particular module and optionally key.
---
---@param module string The module under which the data is stored.
---@param key? string The key under which the data is stored. If omitted, will clear the entire store for this module.
function storage.clear(module, key)
  local ok, err = pcall(native_storage.clear, module, key)
  if not ok then
    core.error("error clearing storage file %s: %s", module_key_to_path(module, key), err)
  end
end


return storage
