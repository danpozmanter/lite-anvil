-- mod-version:4
local core = require "core"
local command = require "core.command"
local common = require "core.common"
local config = require "core.config"
local DocView = require "core.docview"
local Doc = require "core.doc"
local native_affordance = require "affordance_model"

local cache = setmetatable({}, { __mode = "k" })
local auto_detect_max_lines = 150

local function update_cache(doc)
  local result = native_affordance.detect_indent(doc.lines, auto_detect_max_lines, config.indent_size)
  local score_threshold = 2
  local indent_type = result.score >= score_threshold and result.type or config.tab_type
  local indent_size = result.score >= score_threshold and result.size or config.indent_size
  cache[doc] = {
    type = indent_type,
    size = indent_size,
    confirmed = result.score >= score_threshold,
  }
  doc.indent_info = cache[doc]
end

local old_new = Doc.new
function Doc:new(...)
  old_new(self, ...)
  update_cache(self)
end

local old_clean = Doc.clean
function Doc:clean(...)
  old_clean(self, ...)
  local _, _, confirmed = self:get_indent_info()
  if not confirmed then
    update_cache(self)
  end
end

local function set_indent_type(doc, indent_type)
  local _, indent_size = doc:get_indent_info()
  cache[doc] = {
    type = indent_type,
    size = indent_size,
    confirmed = true,
  }
  doc.indent_info = cache[doc]
end

local function set_indent_type_command(dv)
  core.command_view:enter("Specify indent style for this file", {
    submit = function(value)
      set_indent_type(dv.doc, value:lower() == "tabs" and "hard" or "soft")
    end,
    suggest = function(text)
      return common.fuzzy_match({ "tabs", "spaces" }, text)
    end,
    validate = function(text)
      local t = text:lower()
      return t == "tabs" or t == "spaces"
    end
  })
end

local function set_indent_size(doc, indent_size)
  local indent_type = doc:get_indent_info()
  cache[doc] = {
    type = indent_type,
    size = indent_size,
    confirmed = true,
  }
  doc.indent_info = cache[doc]
end

local function set_indent_size_command(dv)
  core.command_view:enter("Specify indent size for current file", {
    submit = function(value)
      set_indent_size(dv.doc, math.floor(tonumber(value)))
    end,
    validate = function(value)
      value = tonumber(value)
      return value ~= nil and value >= 1
    end
  })
end

command.add("core.docview", {
  ["indent:set-file-indent-type"] = set_indent_type_command,
  ["indent:set-file-indent-size"] = set_indent_size_command
})

command.add(function()
  return core.active_view:is(DocView)
    and cache[core.active_view.doc]
    and cache[core.active_view.doc].type == "soft"
end, {
  ["indent:switch-file-to-tabs-indentation"] = function()
    set_indent_type(core.active_view.doc, "hard")
  end
})

command.add(function()
  return core.active_view:is(DocView)
    and cache[core.active_view.doc]
    and cache[core.active_view.doc].type == "hard"
end, {
  ["indent:switch-file-to-spaces-indentation"] = function()
    set_indent_type(core.active_view.doc, "soft")
  end
})
