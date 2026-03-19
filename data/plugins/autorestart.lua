-- mod-version:4
local core = require "core"
local command = require "core.command"
local config = require "core.config"
local Doc = require "core.doc"
local common = require "core.common"
local native_affordance = require "affordance_model"

config.plugins.autorestart = common.merge({}, config.plugins.autorestart)

local save = Doc.save
Doc.save = function(self, ...)
  local res = save(self, ...)
  local ok, err = pcall(function()
    local project = core.root_project and core.root_project()
    if self.abs_filename and native_affordance.should_autorestart(
      self.abs_filename,
      USERDIR,
      PATHSEP,
      project and project.path or nil
    ) then
      command.perform("core:restart")
    end
  end)
  if not ok then
    core.error("Post-save autorestart hook failed for %s: %s", self:get_name(), err)
  end
  return res
end
