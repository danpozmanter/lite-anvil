-- mod-version:4
local common = require "core.common"
local config = require "core.config"
local command = require "core.command"
local Doc = require "core.doc"
local native_affordance = require "affordance_model"

config.plugins.trimwhitespace = common.merge({
  enabled = false,
  trim_empty_end_lines = false,
  config_spec = {
    name = "Trim Whitespace",
    {
      label = "Enabled",
      description = "Disable or enable the trimming of white spaces by default.",
      path = "enabled",
      type = "toggle",
      default = false
    },
    {
      label = "Trim Empty End Lines",
      description = "Remove any empty new lines at the end of documents.",
      path = "trim_empty_end_lines",
      type = "toggle",
      default = false
    }
  }
}, config.plugins.trimwhitespace)

local trimwhitespace = {}

function trimwhitespace.disable(doc)
  doc.disable_trim_whitespace = true
end

function trimwhitespace.enable(doc)
  doc.disable_trim_whitespace = nil
end

function trimwhitespace.trim(doc)
  local cline, ccol = doc:get_selection()
  for i = 1, #doc.lines do
    local old_text = doc:get_text(i, 1, i, math.huge)
    local new_text = native_affordance.trim_line(old_text, cline == i and ccol or nil)
    if old_text ~= new_text then
      doc:insert(i, 1, new_text)
      doc:remove(i, #new_text + 1, i, math.huge)
    end
  end
end

function trimwhitespace.trim_empty_end_lines(doc, raw_remove)
  local count = native_affordance.count_empty_end_lines(doc.lines)
  for _ = 1, count do
    local l = #doc.lines
    if l > 1 and doc.lines[l] == "\n" then
      local current_line = doc:get_selection()
      if current_line == l then
        doc:set_selection(l - 1, math.huge, l - 1, math.huge)
      end
      if not raw_remove then
        doc:remove(l - 1, math.huge, l, math.huge)
      else
        table.remove(doc.lines, l)
      end
    end
  end
end

command.add("core.docview", {
  ["trim-whitespace:trim-trailing-whitespace"] = function(dv)
    trimwhitespace.trim(dv.doc)
  end,
  ["trim-whitespace:trim-empty-end-lines"] = function(dv)
    trimwhitespace.trim_empty_end_lines(dv.doc)
  end,
})

local doc_save = Doc.save
Doc.save = function(self, ...)
  if config.plugins.trimwhitespace.enabled and not self.disable_trim_whitespace then
    trimwhitespace.trim(self)
    if config.plugins.trimwhitespace.trim_empty_end_lines then
      trimwhitespace.trim_empty_end_lines(self)
    end
  end
  return doc_save(self, ...)
end

return trimwhitespace
