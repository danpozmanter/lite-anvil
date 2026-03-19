local root = "/home/daniel/dev/lite-anvil"
local plugins_dir = root .. "/data/plugins"
local assets_dir = root .. "/data/assets/syntax"

local function read_all(path)
  local f = assert(io.open(path, "rb"))
  local data = f:read("*a")
  f:close()
  return data
end

local function write_all(path, data)
  local f = assert(io.open(path, "wb"))
  f:write(data)
  f:close()
end

local function run(cmd)
  local p = assert(io.popen(cmd, "r"))
  local out = p:read("*a")
  local ok, _, code = p:close()
  if ok == nil or code ~= 0 then
    error("command failed: " .. cmd)
  end
  return out
end

local function is_array(t)
  if type(t) ~= "table" then
    return false
  end
  local n = 0
  for k in pairs(t) do
    if type(k) ~= "number" or k < 1 or k % 1 ~= 0 then
      return false
    end
    n = n + 1
  end
  for i = 1, n do
    if t[i] == nil then
      return false
    end
  end
  return true
end

local function json_escape(s)
  return (s:gsub('[%z\1-\31\\"]', function(c)
    local map = {
      ['"'] = '\\"',
      ['\\'] = '\\\\',
      ['\b'] = '\\b',
      ['\f'] = '\\f',
      ['\n'] = '\\n',
      ['\r'] = '\\r',
      ['\t'] = '\\t',
    }
    return map[c] or string.format("\\u%04x", c:byte())
  end))
end

local function encode_json(v)
  local tv = type(v)
  if tv == "nil" then
    return "null"
  elseif tv == "boolean" or tv == "number" then
    return tostring(v)
  elseif tv == "string" then
    return '"' .. json_escape(v) .. '"'
  elseif tv ~= "table" then
    error("unsupported value type: " .. tv)
  end

  if is_array(v) then
    local out = {}
    for i = 1, #v do
      out[i] = encode_json(v[i])
    end
    return "[" .. table.concat(out, ", ") .. "]"
  end

  local out = {}
  for k, item in pairs(v) do
    if type(k) ~= "string" then
      error("unsupported object key type: " .. type(k))
    end
    out[#out + 1] = '"' .. json_escape(k) .. '": ' .. encode_json(item)
  end
  table.sort(out)
  return "{\n  " .. table.concat(out, ",\n  ") .. "\n}"
end

local function normalize(value, seen)
  seen = seen or {}
  local tv = type(value)
  if tv == "nil" or tv == "boolean" or tv == "number" or tv == "string" then
    return value
  end
  if tv ~= "table" then
    error("unsupported runtime type: " .. tv)
  end
  if seen[value] then
    error("cyclic table")
  end
  seen[value] = true

  local out
  if is_array(value) then
    out = {}
    for i = 1, #value do
      out[i] = normalize(value[i], seen)
    end
  else
    out = {}
    for k, item in pairs(value) do
      if type(k) ~= "string" then
        error("unsupported key type: " .. type(k))
      end
      out[k] = normalize(item, seen)
    end
  end
  seen[value] = nil
  return out
end

local function graph_encode(value, state)
  local tv = type(value)
  if tv == "nil" or tv == "boolean" or tv == "number" or tv == "string" then
    return value
  end
  if tv ~= "table" then
    error("unsupported runtime type: " .. tv)
  end

  state = state or { ids = {}, nodes = {}, next_id = 1 }
  if state.ids[value] then
    return { ["$ref"] = state.ids[value] }
  end

  local id = tostring(state.next_id)
  state.next_id = state.next_id + 1
  state.ids[value] = id

  local node = {
    kind = is_array(value) and "array" or "object",
    values = {},
  }
  state.nodes[id] = node

  if node.kind == "array" then
    for i = 1, #value do
      node.values[i] = graph_encode(value[i], state)
    end
  else
    for k, item in pairs(value) do
      if type(k) ~= "string" then
        error("unsupported key type: " .. type(k))
      end
      node.values[k] = graph_encode(item, state)
    end
  end

  return { ["$ref"] = id }, state
end

local function capture_language_table(source, name)
  local captured = nil
  local STOP = {}
  local fake_syntax = {
    add = function(t)
      captured = t
      error(STOP)
    end,
  }
  local fake_style = {
    syntax_fonts = {},
    on_theme_change = {},
  }
  local fake_common = {}
  local fake_command = {}
  local fake_config = {}
  local fake_core = {}
  local fake_regex = {}

  local env = {
    assert = assert,
    error = error,
    ipairs = ipairs,
    math = math,
    next = next,
    pairs = pairs,
    pcall = pcall,
    select = select,
    string = string,
    table = table,
    tonumber = tonumber,
    tostring = tostring,
    type = type,
    utf8 = utf8,
    require = function(mod)
      if mod == "core.syntax" then return fake_syntax end
      if mod == "core.style" then return fake_style end
      if mod == "core.common" then return fake_common end
      if mod == "core.command" then return fake_command end
      if mod == "core.config" then return fake_config end
      if mod == "core" then return fake_core end
      if mod == "core.regex" then return fake_regex end
      return {}
    end,
  }

  local chunk, err = load(source, "@" .. name, "t", env)
  if not chunk then
    return nil, err
  end
  local ok, run_err = pcall(chunk)
  if not ok and run_err ~= STOP then
    return nil, run_err
  end
  if not captured then
    return nil, "no syntax.add table captured"
  end
  local ok_graph, root_ref, state = pcall(graph_encode, captured)
  if ok_graph then
    return {
      syntax = {
        graph = {
          nodes = state.nodes,
        },
        root = root_ref,
      }
    }
  end
  return normalize({ syntax = captured })
end

local function export_language(name)
  local rel = "data/plugins/" .. name .. ".lua"
  local source = run(string.format("cd %q && git show v0.14.7:%s", root, rel))
  local ok, result = pcall(capture_language_table, source, name)
  if not ok or not result then
    io.stderr:write(string.format("SKIP %s: %s\n", name, result or ok))
    return false
  end
  local asset_name = name:gsub("^language_", "")
  local json = encode_json(result) .. "\n"
  write_all(assets_dir .. "/" .. asset_name .. ".json", json)
  return true
end

local names = {}
for file in io.popen(string.format("cd %q && find data/plugins -maxdepth 1 -name 'language_*.lua' -printf '%%f\\n' | sed 's/\\.lua$//' | sort", root)):lines() do
  names[#names + 1] = file
end

local ok_count = 0
for _, name in ipairs(names) do
  if export_language(name) then
    ok_count = ok_count + 1
  end
end

io.stdout:write(string.format("exported %d/%d language assets\n", ok_count, #names))
