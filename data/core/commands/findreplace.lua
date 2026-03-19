local core = require "core"
local command = require "core.command"
local config = require "core.config"
local search = require "core.doc.search"
local keymap = require "core.keymap"
local style = require "core.style"
local DocView = require "core.docview"
local CommandView = require "core.commandview"
local StatusView = require "core.statusview"
local doc_native = require "doc_native"

local last_view, last_fn, last_text, last_sel

local case_sensitive = config.find_case_sensitive or false
local find_regex = config.find_regex or false
local whole_word = config.find_whole_word or false
local found_expression
local find_ui_active = false

local function doc()
  local is_DocView = core.active_view:is(DocView) and not core.active_view:is(CommandView)
  return is_DocView and core.active_view.doc or (last_view and last_view.doc)
end

local function get_find_tooltip()
  local rf = keymap.get_binding("find-replace:repeat-find")
  local sa = keymap.get_binding("find-replace:select-all-found")
  local ti = keymap.get_binding("find-replace:toggle-sensitivity")
  local tr = keymap.get_binding("find-replace:toggle-regex")
  local tw = keymap.get_binding("find-replace:toggle-whole-word")
  return (find_regex and "[Regex] " or "") ..
    (case_sensitive and "[Sensitive] " or "") ..
    (whole_word and "[Whole Word] " or "") ..
    (rf and ("Press " .. rf .. " to select the next match.") or "") ..
    (sa and (" " .. sa .. " selects all matches as multi-cursors.") or "") ..
    (ti and (" " .. ti .. " toggles case sensitivity.") or "") ..
    (tr and (" " .. tr .. " toggles regex find.") or "") ..
    (tw and (" " .. tw .. " toggles whole word.") or "")
end

local function update_preview(sel, search_fn, text)
  local ok, line1, col1, line2, col2 = pcall(search_fn, last_view.doc,
    sel[1], sel[2], text, case_sensitive, find_regex, false, whole_word)
  if ok and line1 and text ~= "" then
    last_view.doc:set_selection(line2, col2, line1, col1)
    last_view:scroll_to_line(line2, true)
    found_expression = true
  else
    last_view.doc:set_selection(table.unpack(sel))
    found_expression = false
  end
end


local function insert_unique(t, v)
  local n = #t
  for i = 1, n do
    if t[i] == v then
      table.remove(t, i)
      break
    end
  end
  table.insert(t, 1, v)
end


local function find(label, search_fn)
  last_view, last_sel = core.active_view,
    { core.active_view.doc:get_selection() }
  local text = last_view.doc:get_text(table.unpack(last_sel))
  found_expression = false
  find_ui_active = true

  core.status_view:show_tooltip(get_find_tooltip())

  core.command_view:enter(label, {
    text = text,
    select_text = true,
    show_suggestions = false,
    submit = function(text, item)
      insert_unique(core.previous_find, text)
      core.status_view:remove_tooltip()
      find_ui_active = false
      if found_expression then
        last_fn, last_text = search_fn, text
      else
        core.error("Couldn't find %q", text)
        last_view.doc:set_selection(table.unpack(last_sel))
        last_view:scroll_to_make_visible(table.unpack(last_sel))
      end
    end,
    suggest = function(text)
      update_preview(last_sel, search_fn, text)
      last_fn, last_text = search_fn, text
      return core.previous_find
    end,
    cancel = function(explicit)
      core.status_view:remove_tooltip()
      find_ui_active = false
      if explicit then
        last_view.doc:set_selection(table.unpack(last_sel))
        last_view:scroll_to_make_visible(table.unpack(last_sel))
      end
    end
  })
end


local function replace(kind, default, fn)
  core.status_view:show_tooltip(get_find_tooltip())
  find_ui_active = true
  core.command_view:enter("Find To Replace " .. kind, {
    text = default,
    select_text = true,
    show_suggestions = false,
    submit = function(old)
      insert_unique(core.previous_find, old)

      local s = string.format("Replace %s %q With", kind, old)
      core.command_view:enter(s, {
        text = old,
        select_text = true,
        show_suggestions = false,
        submit = function(new)
          core.status_view:remove_tooltip()
          find_ui_active = false
          insert_unique(core.previous_replace, new)
          local results = doc():replace(function(text)
            return fn(text, old, new)
          end)
          local n = 0
          for _,v in pairs(results) do
            n = n + v
          end
          core.log("Replaced %d instance(s) of %s %q with %q", n, kind, old, new)
        end,
        suggest = function() return core.previous_replace end,
        cancel = function()
          core.status_view:remove_tooltip()
          find_ui_active = false
        end
      })
    end,
    suggest = function() return core.previous_find end,
    cancel = function()
      core.status_view:remove_tooltip()
      find_ui_active = false
    end
  })
end

local function native_replace_text(text, old, new, regex_mode)
  local result = doc_native.replace(text, old, new, {
    regex = regex_mode and true or false,
  })
  return result.text, result.count
end

local function has_selection()
  return core.active_view:is(DocView) and core.active_view.doc:has_selection()
end

local function has_unique_selection()
  if not doc() then return false end
  local text = nil
  for idx, line1, col1, line2, col2 in doc():get_selections(true, true) do
    if line1 == line2 and col1 == col2 then return false end
    local selection = doc():get_text(line1, col1, line2, col2)
    if text ~= nil and text ~= selection then return false end
    text = selection
  end
  return text ~= nil
end

local function is_in_selection(line, col, l1, c1, l2, c2)
  if line < l1 or line > l2 then return false end
  if line == l1 and col <= c1 then return false end
  if line == l2 and col > c2 then return false end
  return true
end

local function is_in_any_selection(line, col)
  for idx, l1, c1, l2, c2 in doc():get_selections(true, false) do
    if is_in_selection(line, col, l1, c1, l2, c2) then return true end
  end
  return false
end

local function select_add_next(all)
  local il1, ic1
  for _, l1, c1, l2, c2 in doc():get_selections(true, true) do
    if not il1 then
      il1, ic1 = l1, c1
    end
    local text = doc():get_text(l1, c1, l2, c2)
    repeat
      l1, c1, l2, c2 = search.find(doc(), l2, c2, text, { wrap = true })
      if l1 == il1 and c1 == ic1 then break end
      if l2 and not is_in_any_selection(l2, c2) then
        doc():add_selection(l2, c2, l1, c1)
        if not all then
          core.active_view:scroll_to_make_visible(l2, c2)
          return
        end
      end
    until not all or not l2
    if all then break end
  end
end

local function select_next(reverse)
  local l1, c1, l2, c2 = doc():get_selection(true)
  local text = doc():get_text(l1, c1, l2, c2)
  if reverse then
    l1, c1, l2, c2 = search.find(doc(), l1, c1, text, { wrap = true, reverse = true, whole_word = whole_word })
  else
    l1, c1, l2, c2 = search.find(doc(), l2, c2, text, { wrap = true, whole_word = whole_word })
  end
  if l2 then doc():set_selection(l2, c2, l1, c1) end
end


local function select_all_found(dv)
  if not last_text or last_text == "" then
    core.error("No find text to convert into multi-cursors")
    return
  end

  local matches = {}
  local line, col = 1, 1
  local opt = {
    no_case = not case_sensitive,
    regex = find_regex,
    whole_word = whole_word,
  }

  while true do
    local l1, c1, l2, c2 = search.find(dv.doc, line, col, last_text, opt)
    if not l1 then break end

    table.insert(matches, { l2, c2, l1, c1 })

    local next_line, next_col = l2, c2
    if l1 == l2 and c1 == c2 then
      next_line, next_col = dv.doc:position_offset(l2, c2, 1)
      if next_line == l2 and next_col == c2 then
        break
      end
    end
    line, col = next_line, next_col
  end

  if #matches == 0 then
    core.error("Couldn't find %q", last_text)
    return
  end

  dv.doc:set_selection(table.unpack(matches[1]))
  for i = 2, #matches do
    dv.doc:add_selection(table.unpack(matches[i]))
  end
  dv:scroll_to_line(matches[1][3], true)
  core.status_view:show_message("i", style.text, string.format("%d selection(s) active", #matches))
end

---@param in_selection? boolean whether to replace in the selections only, or in the whole file.
local function find_replace(in_selection)
  local l1, c1, l2, c2 = doc():get_selection()
  local selected_text = ""
  if not in_selection then
    selected_text = doc():get_text(l1, c1, l2, c2)
    doc():set_selection(l2, c2, l2, c2)
  end
  replace("Text", l1 == l2 and selected_text or "", function(text, old, new)
    if not find_regex then
      local native_text, native_count = native_replace_text(text, old, new, false)
      if native_text ~= nil then
        return native_text, native_count
      end
      return text:gsub(old:gsub("%W", "%%%1"), new:gsub("%%", "%%%%"), nil)
    end
    local native_text, native_count = native_replace_text(text, old, new, true)
    if native_text ~= nil then
      return native_text, native_count
    end
    local result, matches = regex.gsub(regex.compile(old, "m"), text, new)
    return result, matches
  end)
end

command.add(has_unique_selection, {
  ["find-replace:select-next"] = select_next,
  ["find-replace:select-previous"] = function() select_next(true) end,
  ["find-replace:select-add-next"] = select_add_next,
  ["find-replace:select-add-all"] = function() select_add_next(true) end
})

command.add("core.docview!", {
  ["find-replace:find"] = function()
    find("Find Text", function(doc, line, col, text, case_sensitive, find_regex, find_reverse)
      local opt = { wrap = true, no_case = not case_sensitive, regex = find_regex, reverse = find_reverse, whole_word = whole_word }
      return search.find(doc, line, col, text, opt)
    end)
  end,

  ["find-replace:replace"] = function()
    find_replace()
  end,

  ["find-replace:replace-in-selection"] = function()
    find_replace(true)
  end,

  ["find-replace:replace-symbol"] = function()
    local first = ""
    if doc():has_selection() then
      local text = doc():get_text(doc():get_selection())
      first = text:match(config.symbol_pattern) or ""
    end
    replace("Symbol", first, function(text, old, new)
      local n = 0
      local res = text:gsub(config.symbol_pattern, function(sym)
        if old == sym then
          n = n + 1
          return new
        end
      end)
      return res, n
    end)
  end,
})

local function valid_for_finding()
  -- Allow using this while in the CommandView
  if core.active_view:is(CommandView) and last_view then
    return true, last_view
  end
  return core.active_view:is(DocView), core.active_view
end

command.add(valid_for_finding, {
  ["find-replace:repeat-find"] = function(dv)
    if not last_fn then
      core.error("No find to continue from")
    else
      local sl1, sc1, sl2, sc2 = dv.doc:get_selection(true)
      local line1, col1, line2, col2 = last_fn(dv.doc, sl2, sc2, last_text, case_sensitive, find_regex, false, whole_word)
      if line1 then
        dv.doc:set_selection(line2, col2, line1, col1)
        dv:scroll_to_line(line2, true)
      else
        core.error("Couldn't find %q", last_text)
      end
    end
  end,

  ["find-replace:previous-find"] = function(dv)
    if not last_fn then
      core.error("No find to continue from")
    else
      local sl1, sc1, sl2, sc2 = dv.doc:get_selection(true)
      local line1, col1, line2, col2 = last_fn(dv.doc, sl1, sc1, last_text, case_sensitive, find_regex, true, whole_word)
      if line1 then
        dv.doc:set_selection(line2, col2, line1, col1)
        dv:scroll_to_line(line2, true)
      else
        core.error("Couldn't find %q", last_text)
      end
    end
  end,

  ["find-replace:select-all-found"] = function(dv)
    select_all_found(dv)
  end,
})

command.add("core.commandview", {
  ["find-replace:toggle-sensitivity"] = function()
    case_sensitive = not case_sensitive
    core.status_view:show_tooltip(get_find_tooltip())
    if last_sel then update_preview(last_sel, last_fn, last_text) end
  end,

  ["find-replace:toggle-regex"] = function()
    find_regex = not find_regex
    core.status_view:show_tooltip(get_find_tooltip())
    if last_sel then update_preview(last_sel, last_fn, last_text) end
  end,

  ["find-replace:toggle-whole-word"] = function()
    whole_word = not whole_word
    core.status_view:show_tooltip(get_find_tooltip())
    if last_sel then update_preview(last_sel, last_fn, last_text) end
  end
})

core.status_view:add_item({
  predicate = function()
    return find_ui_active and core.active_view and core.active_view:is(CommandView)
  end,
  name = "find:state",
  alignment = StatusView.Item.RIGHT,
  get_item = function()
    return {
      case_sensitive and style.accent or style.dim, "Aa",
      style.dim, " ",
      find_regex and style.accent or style.dim, ".*",
      style.dim, " ",
      whole_word and style.accent or style.dim, "W",
    }
  end,
  tooltip = "Search toggles: case, regex, whole word"
})
