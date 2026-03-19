-- mod-version:4
local core = require "core"
local command = require "core.command"
local common = require "core.common"
local config = require "core.config"
local keymap = require "core.keymap"
local style = require "core.style"
local native_picker = require "picker"
local TreeView = require "plugins.treeview"

local git = require ".status"
local ui = require ".ui"

local function active_path()
  local view = core.active_view
  if view and view.doc and view.doc.abs_filename then
    return view.doc.abs_filename
  end
  local project = core.root_project and core.root_project()
  return project and project.path or nil
end

local function show_git_result(ok, stdout, stderr, success)
  if ok then
    if success then
      core.status_view:show_message("i", style.text, success)
    end
  else
    core.error(stderr ~= "" and stderr or "Git command failed")
  end
end

local function refresh_active(force)
  return git.refresh(active_path(), force)
end

local function with_selected_file(fn)
  local view = core.active_view
  if view and view.context == "session" and view.get_selected then
    local item = view:get_selected()
    if item then
      fn(item)
      return
    end
  end
  local path = active_path()
  local entry = path and git.get_file_status(path)
  if entry then
    fn(entry)
  else
    core.error("No Git-tracked change selected")
  end
end

local function prompt_branch_checkout()
  git.list_branches(active_path(), function(branches, err)
    if not branches then
      core.error(err or "Unable to list branches")
      return
    end
    core.command_view:enter("Checkout Branch", {
      suggest = function(text)
        if not text or text == "" then
          return branches
        end
        return native_picker.rank_strings(branches, text)
      end,
      submit = function(text, item)
        local branch = item and item.text or text
        if branch == "" then
          return
        end
        git.run(active_path(), { "checkout", branch }, function(ok, stdout, stderr)
          show_git_result(ok, stdout, stderr, "Checked out " .. branch)
        end)
      end
    })
  end)
end

local function prompt_branch_create()
  core.command_view:enter("Create Branch", {
    submit = function(text)
      if text == "" then
        return
      end
      git.run(active_path(), { "checkout", "-b", text }, function(ok, stdout, stderr)
        show_git_result(ok, stdout, stderr, "Created branch " .. text)
      end)
    end
  })
end

local function prompt_commit()
  core.command_view:enter("Commit Message", {
    submit = function(text)
      if text == "" then
        return
      end
      git.run(active_path(), { "commit", "-m", text }, function(ok, stdout, stderr)
        show_git_result(ok, stdout, stderr, "Committed changes")
      end)
    end
  })
end

local function prompt_stash()
  core.command_view:enter("Stash Message (optional)", {
    submit = function(text)
      local args = { "stash", "push" }
      if text ~= "" then
        args[#args + 1] = "-m"
        args[#args + 1] = text
      end
      git.run(active_path(), args, function(ok, stdout, stderr)
        show_git_result(ok, stdout, stderr, "Stashed changes")
      end)
    end
  })
end

command.add(nil, {
  ["git:status"] = function()
    ui.open_status()
  end,
  ["git:refresh"] = function()
    refresh_active(true)
  end,
  ["git:commit"] = function()
    prompt_commit()
  end,
  ["git:pull"] = function()
    git.run(active_path(), { "pull", "--ff-only" }, function(ok, stdout, stderr)
      show_git_result(ok, stdout, stderr, "Pulled latest changes")
    end)
  end,
  ["git:push"] = function()
    git.run(active_path(), { "push" }, function(ok, stdout, stderr)
      show_git_result(ok, stdout, stderr, "Pushed changes")
    end)
  end,
  ["git:checkout"] = function()
    prompt_branch_checkout()
  end,
  ["git:branch"] = function()
    prompt_branch_create()
  end,
  ["git:stash"] = function()
    prompt_stash()
  end,
  ["git:diff-repo"] = function()
    ui.open_repo_diff(nil, false)
  end,
  ["git:diff-repo-staged"] = function()
    ui.open_repo_diff(nil, true)
  end,
  ["git:diff-file"] = function()
    with_selected_file(function(item)
      ui.open_file_diff(item.path, item.kind == "staged")
    end)
  end,
  ["git:stage-file"] = function()
    with_selected_file(function(item)
      git.stage(item.path, function(ok, stdout, stderr)
        show_git_result(ok, stdout, stderr, "Staged " .. item.rel)
      end)
    end)
  end,
  ["git:unstage-file"] = function()
    with_selected_file(function(item)
      git.unstage(item.path, function(ok, stdout, stderr)
        show_git_result(ok, stdout, stderr, "Unstaged " .. item.rel)
      end)
    end)
  end,
})

command.add(ui.StatusView, {
  ["git:select-next"] = function()
    local view = core.active_view
    view.selected_idx = math.min(view.selected_idx + 1, #view:get_items())
    view:scroll_to_selected()
  end,
  ["git:select-previous"] = function()
    local view = core.active_view
    view.selected_idx = math.max(view.selected_idx - 1, 1)
    view:scroll_to_selected()
  end,
  ["git:open-selected"] = function()
    core.active_view:open_selected()
  end,
})

keymap.add {
  ["ctrl+shift+g"] = "git:status",
  ["return"] = "git:open-selected",
  ["up"] = "git:select-previous",
  ["down"] = "git:select-next",
}

if not TreeView.__git_highlighting_patched then
  TreeView.__git_highlighting_patched = true
  local get_item_text = TreeView.get_item_text
  function TreeView:get_item_text(item, active, hovered)
    local text, font, color = get_item_text(self, item, active, hovered)
    if not active and not hovered and not item.ignored and item.type == "file"
        and config.plugins.git.treeview_highlighting ~= false then
      local entry = git.get_file_status(item.abs_filename)
      if entry then
        if entry.kind == "staged" then
          color = style.accent
        elseif entry.kind == "untracked" then
          color = style.good or color
        elseif entry.kind == "conflict" then
          color = style.error or color
        else
          color = style.text
        end
      end
    end
    return text, font, color
  end
end

return {
  status = git,
  ui = ui,
}
