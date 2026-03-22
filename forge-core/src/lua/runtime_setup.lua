
package.native_plugins = package.native_plugins or {}
package.searchers = { package.searchers[1], package.searchers[2], function(modname)
  local path, err = package.searchpath(modname, package.cpath)
  if not path then return err end
  return system.load_native_plugin, path
end }

table.pack = table.pack or pack or function(...) return {...} end
table.unpack = table.unpack or unpack

local lua_require = require
local require_stack = { "" }

function require(modname, ...)
  if modname then
    local level, rel_path = string.match(modname, "^(%.*)(.*)")
    level = #(level or "")
    if level > 0 then
      if #require_stack == 0 then
        return error("Require stack underflowed.")
      else
        local base_path = require_stack[#require_stack]
        while level > 1 do
          base_path = string.match(base_path, "^(.*)%.") or ""
          level = level - 1
        end
        modname = base_path
        if #base_path > 0 then
          modname = modname .. "."
        end
        modname = modname .. rel_path
      end
    end
  end

  table.insert(require_stack, modname)
  local ok, result, loaderdata = pcall(lua_require, modname, ...)
  table.remove(require_stack)

  if not ok then
    return error(result)
  end
  return result, loaderdata
end

function get_current_require_path()
  return require_stack[#require_stack]
end

require "core.utf8string"
require "core.process"
