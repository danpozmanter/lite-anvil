local common = require "core.common"
local core   = require "core"
local native_tokenizer = require "native_tokenizer"
local json = require "plugins.lsp.json"

local syntax = {}
syntax.items = {}
syntax.lazy_items = {}
syntax.lazy_loaded = {}
syntax.loaded_assets = {}

syntax.plain_text_syntax = { name = "Plain Text", patterns = {}, symbols = {} }

if native_tokenizer.register_syntax then
  pcall(native_tokenizer.register_syntax, "Plain Text", syntax.plain_text_syntax)
end


---Checks whether the pattern / regex compiles correctly and matches something.
---A pattern / regex must not match an empty string.
---@param pattern_type "regex"|"pattern"
---@param pattern string
---@return boolean ok
---@return string? error
local function check_pattern(pattern_type, pattern)
  local ok, err, mstart, mend
  if pattern_type == "regex" then
    ok, err = regex.compile(pattern)
    if ok then
      mstart, mend = regex.find_offsets(ok, "")
      if mstart and mstart > mend then
        ok, err = false, "Regex matches an empty string"
      end
    end
  else
    ok, mstart, mend = pcall(string.ufind, "", pattern)
    if ok and mstart and mstart > mend then
      ok, err = false, "Pattern matches an empty string"
    elseif not ok then
      err = mstart --[[@as string]]
    end
  end
  return ok --[[@as boolean]], err
end

function syntax.add(t)
  if type(t.space_handling) ~= "boolean" then t.space_handling = true end

  if t.patterns then
    -- do a sanity check on the patterns / regex to make sure they are actually correct
    for i, pattern in ipairs(t.patterns) do
      local p, ok, err, name = pattern.pattern or pattern.regex, nil, nil, nil
      if type(p) == "table" then
        for j = 1, 2 do
          ok, err = check_pattern(pattern.pattern and "pattern" or "regex", p[j])
          if not ok then name = string.format("#%d:%d <%s>", i, j, p[j]) end
        end
      elseif type(p) == "string" then
        ok, err = check_pattern(pattern.pattern and "pattern" or "regex", p)
        if not ok then name = string.format("#%d <%s>", i, p) end
      else
        ok, err, name = false, "Missing pattern or regex", "#"..i
      end
      if not ok then
        pattern.disabled = true
        core.warn("Malformed pattern %s in %s language plugin: %s", name, t.name, err)
      end
    end

    -- the rule %s+ gives us a performance gain for the tokenizer in lines with
    -- long amounts of consecutive spaces, can be disabled by plugins where it
    -- causes conflicts by declaring the table property: space_handling = false
    if t.space_handling then
      table.insert(t.patterns, { pattern = "%s+", type = "normal" })
    end

    -- this rule gives us additional performance gain by matching every word
    -- that was not matched by the syntax patterns as a single token, preventing
    -- the tokenizer from iterating over each character individually which is a
    -- lot slower since iteration occurs in lua instead of C and adding to that
    -- it will also try to match every pattern to a single char (same as spaces)
    table.insert(t.patterns, { pattern = "%w+%f[%s]", type = "normal" })
  end

  table.insert(syntax.items, t)

  if native_tokenizer.available and t.name then
    local ok, err = pcall(native_tokenizer.register_syntax, t.name, t)
    if not ok then
      core.warn("Failed to register %s with native tokenizer: %s", t.name, err)
    end
  end
end

local function read_text_file(path)
  local file = io.open(path, "r")
  if not file then
    return nil
  end
  local content = file:read("*a")
  file:close()
  return content
end

local function resolve_asset_path(asset)
  for _, root in ipairs { USERDIR, DATADIR } do
    local path = root .. PATHSEP .. "assets" .. PATHSEP .. "syntax" .. PATHSEP .. asset .. ".json"
    if system.get_file_info(path) then
      return path
    end
  end
  return nil
end

local function rebuild_graph_value(graph, value, built)
  if type(value) ~= "table" then
    return value
  end
  if value["$ref"] then
    local id = tostring(value["$ref"])
    if built[id] then
      return built[id]
    end
    local node = assert(graph.nodes and graph.nodes[id], "missing syntax graph node " .. id)
    local out = {}
    built[id] = out
    if node.kind == "array" then
      for i, item in ipairs(node.values or {}) do
        out[i] = rebuild_graph_value(graph, item, built)
      end
    else
      for key, item in pairs(node.values or {}) do
        out[key] = rebuild_graph_value(graph, item, built)
      end
    end
    return out
  end
  local out = {}
  for key, item in pairs(value) do
    out[key] = rebuild_graph_value(graph, item, built)
  end
  return out
end

function syntax.add_from_asset(asset)
  local path = resolve_asset_path(asset)
  if not path then
    return nil, "missing asset"
  end

  local source = read_text_file(path)
  if not source then
    return nil, "unreadable asset"
  end

  local ok, decoded = json.decode_safe(source)
  if not ok or type(decoded) ~= "table" then
    return nil, "invalid asset"
  end

  local payload = decoded.syntax or decoded
  if type(payload) == "table" and payload.graph and payload.root then
    payload = rebuild_graph_value(payload.graph, payload.root, {})
  end

  if syntax.loaded_assets[asset] then
    return true
  end
  syntax.loaded_assets[asset] = true
  syntax.add(payload)
  return true
end

local function decode_asset_payload(asset)
  local path = resolve_asset_path(asset)
  if not path then
    return nil
  end

  local source = read_text_file(path)
  if not source then
    return nil
  end

  local ok, decoded = json.decode_safe(source)
  if not ok or type(decoded) ~= "table" then
    return nil
  end

  local payload = decoded.syntax or decoded
  if type(payload) == "table" and payload.graph and payload.root then
    payload = rebuild_graph_value(payload.graph, payload.root, {})
  end
  return type(payload) == "table" and payload or nil
end


local function find(string, field)
  local best_match = 0
  local best_syntax
  for i = #syntax.items, 1, -1 do
    local t = syntax.items[i]
    local s, e = common.match_pattern(string, t[field] or {})
    if s and e - s > best_match then
      best_match = e - s
      best_syntax = t
    end
  end
  return best_syntax
end

local function extract_match_list(source, field)
  local list = {}
  local block = source:match(field .. "%s*=%s*%b{}")
  if not block then
    return list
  end

  for quote, text in block:gmatch("(['\"])(.-)%1") do
    list[#list + 1] = text
  end

  return list
end

local function should_load_lazy_plugin(entry, filename, header)
  return (filename and common.match_pattern(filename, entry.files))
      or (header and common.match_pattern(header, entry.headers))
end

local function load_lazy_plugin(entry)
  if syntax.lazy_loaded[entry.name] then
    return true
  end

  syntax.lazy_loaded[entry.name] = true
  local ok, res = core.try(entry.load, entry.plugin)
  if ok then
    return res
  end
  return nil
end

function syntax.register_lazy_plugin(plugin)
  local files = {}
  local headers = {}
  local metadata_path = plugin.file:gsub("%.lua$", ".lazy.json")
  local metadata = read_text_file(metadata_path)
  if metadata and json and json.decode_safe then
    local ok, decoded = json.decode_safe(metadata)
    if ok and type(decoded) == "table" then
      files = decoded.files or {}
      headers = decoded.headers or {}
    end
  end

  if #files == 0 and #headers == 0 then
    local file = io.open(plugin.file, "r")
    if not file then
      return
    end

    local source = file:read("*a")
    file:close()

    files = extract_match_list(source, "files")
    headers = extract_match_list(source, "headers")
  end

  syntax.lazy_items[#syntax.lazy_items + 1] = {
    name = plugin.name,
    plugin = plugin,
    load = plugin.load,
    files = files,
    headers = headers,
  }
end

local function register_lazy_asset(asset)
  if asset == "md" then
    return
  end

  local payload = decode_asset_payload(asset)
  if not payload then
    return
  end

  syntax.lazy_items[#syntax.lazy_items + 1] = {
    name = "asset:" .. asset,
    load = function()
      return syntax.add_from_asset(asset)
    end,
    files = payload.files or {},
    headers = payload.headers or {},
  }
end

local function register_builtin_assets()
  local seen = {}
  for _, root in ipairs { USERDIR, DATADIR } do
    local syntax_dir = root .. PATHSEP .. "assets" .. PATHSEP .. "syntax"
    for _, filename in ipairs(system.list_dir(syntax_dir) or {}) do
      local asset = filename:match("^(.*)%.json$")
      if asset and not seen[asset] then
        seen[asset] = true
        register_lazy_asset(asset)
      end
    end
  end
end

function syntax.get(filename, header)
  local loaded = (filename and find(filename, "files"))
      or (header and find(header, "headers"))
  if loaded then
    return loaded
  end

  for i = #syntax.lazy_items, 1, -1 do
    local entry = syntax.lazy_items[i]
    if should_load_lazy_plugin(entry, filename, header) then
      table.remove(syntax.lazy_items, i)
      load_lazy_plugin(entry)
      local lazy_loaded = (filename and find(filename, "files"))
          or (header and find(header, "headers"))
      if lazy_loaded then
        return lazy_loaded
      end
    end
  end

  return syntax.plain_text_syntax
end

register_builtin_assets()

return syntax
