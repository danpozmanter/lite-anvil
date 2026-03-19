use mlua::prelude::*;

const BOOTSTRAP: &str = r##"local Object = require "core.object"
local Highlighter = require ".highlighter"
local translate = require ".translate"
local core = require "core"
local syntax = require "core.syntax"
local config = require "core.config"
local common = require "core.common"
local style = require "core.style"
local doc_native = require "doc_native"

---@class core.doc : core.object
local Doc = Object:extend()

function Doc:__tostring() return "Doc" end

local function show_read_only_message(self)
  if self._read_only_warned then
    return
  end
  self._read_only_warned = true
  core.status_view:show_message("!", style.warn, self:get_name() .. " is read-only")
end

local function split_lines(text)
  local res = {}
  for line in (text .. "\n"):gmatch("(.-)\n") do
    table.insert(res, line)
  end
  return res
end

local function ensure_native_buffer(self)
  if not self.buffer_id then
    self.buffer_id = doc_native.buffer_new()
  end
end

local function sync_native_selections(self)
  if self.buffer_id then
    doc_native.buffer_set_selections(self.buffer_id, self.selections)
  end
end

local function apply_native_snapshot(self, snapshot)
  if not snapshot then
    return
  end
  self.lines = snapshot.lines
  self.selections = snapshot.selections
  self.undo_stack.idx = snapshot.change_id
  self.redo_stack.idx = snapshot.change_id
  self.crlf = snapshot.crlf
end

local function content_signature(lines)
  local hash = 2166136261
  for _, line in ipairs(lines or {}) do
    for i = 1, #line do
      hash = ((hash ~ line:byte(i)) * 16777619) % 4294967296
    end
    hash = ((hash ~ 10) * 16777619) % 4294967296
  end
  return hash
end


function Doc:new(filename, abs_filename, new_file, options)
  options = options or {}
  self.large_file_mode = options.large_file == true
  self.large_file_size = options.file_size
  self.hard_limited = options.hard_limited == true
  self.read_only = options.read_only == true
  self.plain_text_mode = options.plain_text == true
  self.new_file = new_file
  self:reset()
  if filename then
    self:set_filename(filename, abs_filename)
    if not new_file and not options.lazy_restore then
      self:load(abs_filename)
    elseif not new_file and options.lazy_restore then
      self.deferred_load = abs_filename
    end
  end
  if new_file then
    self.crlf = config.line_endings == "crlf"
  end
end

function Doc:ensure_loaded()
  if not self.deferred_load then
    return true
  end
  local filename = self.deferred_load
  self.deferred_load = nil
  local ok, err = self:load(filename)
  if ok then
    self.new_file = false
    self:clean()
    return true
  end
  return nil, err
end

function Doc:reset()
  ensure_native_buffer(self)
  self.lines = { "\n" }
  self.selections = { 1, 1, 1, 1 }
  self.last_selection = 1
  self.undo_stack = { idx = 1 }
  self.redo_stack = { idx = 1 }
  self.clean_change_id = 1
  self.clean_signature = content_signature(self.lines)
  self._signature_cache = { change_id = 1, signature = self.clean_signature }
  self.highlighter = Highlighter(self)
  self.overwrite = false
  self._read_only_warned = false
  if self.buffer_id then
    apply_native_snapshot(self, doc_native.buffer_reset(self.buffer_id))
  end
  self:reset_syntax()
end

function Doc:reset_syntax()
  if self.plain_text_mode then
    self.syntax = syntax.plain_text_syntax
    self.highlighter:soft_reset()
    return
  end
  local header = self:get_text(1, 1, self:position_offset(1, 1, 128))
  local path = self.abs_filename
  if not path and self.filename then
    local root_project = core.root_project and core.root_project()
    if root_project and root_project.path then
      path = root_project.path .. PATHSEP .. self.filename
    else
      path = self.filename
    end
  end
  if path then path = common.normalize_path(path) end
  local syn = syntax.get(path, header)
  if self.syntax ~= syn then
    self.syntax = syn
    self.highlighter:soft_reset()
  end
end

function Doc:set_filename(filename, abs_filename)
  self.filename = filename
  self.abs_filename = abs_filename
  self:reset_syntax()
end

function Doc:load(filename)
  ensure_native_buffer(self)
  self:reset()
  local ok, snapshot = pcall(doc_native.buffer_load, self.buffer_id, filename)
  if not ok or not snapshot then
    self:reset()
    core.error("Cannot open file %s: %s", filename, snapshot or "unknown error")
    return nil, snapshot
  end
  apply_native_snapshot(self, snapshot)
  for i = 1, #self.lines do
    self.highlighter.lines[i] = false
  end
  self:reset_syntax()
  return true
end

function Doc:reload()
  self:ensure_loaded()
  if self.filename then
    local sel = { self:get_selection() }
    self:load(self.abs_filename)
    self:clean()
    self:set_selection(table.unpack(sel))
  end
end

function Doc:save(filename, abs_filename)
  self:ensure_loaded()
  if self.read_only then
    show_read_only_message(self)
    return
  end
  if not filename then
    if not self.filename then
      error("no filename set to default to")
    end
    filename = self.filename
    abs_filename = self.abs_filename
  elseif not self.filename and not abs_filename then
    error("calling save on unnamed doc without absolute path")
  end

  local filename_changed = filename ~= self.filename or abs_filename ~= self.abs_filename

  doc_native.buffer_save(self.buffer_id, abs_filename, self.crlf)
  if filename_changed then
    self:set_filename(filename, abs_filename)
  end
  self.new_file = false
  self:clean()
end

function Doc:get_name()
  return self.filename or "unsaved"
end

function Doc:is_dirty()
  if self.new_file then
    if self.filename then return true end
    return #self.lines > 1 or #self.lines[1] > 1
  end
  local change_id = self:get_change_id()
  if self.clean_change_id == change_id then
    return false
  end
  return self.clean_signature ~= self:get_content_signature(change_id)
end

function Doc:clean()
  self.clean_change_id = self:get_change_id()
  self.clean_signature = self:get_content_signature(self.clean_change_id)
end

function Doc:get_content_signature(change_id)
  change_id = change_id or self:get_change_id()
  local cached = self._signature_cache
  if cached and cached.change_id == change_id then
    return cached.signature
  end
  local signature = content_signature(self.lines)
  self._signature_cache = {
    change_id = change_id,
    signature = signature,
  }
  return signature
end

function Doc:get_indent_info()
  if not self.indent_info then return config.tab_type, config.indent_size, false end
  return self.indent_info.type or config.tab_type,
      self.indent_info.size or config.indent_size,
      self.indent_info.confirmed
end

function Doc:get_change_id()
  return doc_native.buffer_get_change_id(self.buffer_id)
end

local function sort_positions(line1, col1, line2, col2)
  if line1 > line2 or line1 == line2 and col1 > col2 then
    return line2, col2, line1, col1, true
  end
  return line1, col1, line2, col2, false
end

-- Cursor indices are *only* valid during a get_selections() call.
-- Cursors are iterated top to bottom and can never swap positions through
-- normal operation; they can only merge, split, or change order.
function Doc:get_selection(sort)
  local line1, col1, line2, col2, swap = self:get_selection_idx(self.last_selection, sort)
  if not line1 then
    line1, col1, line2, col2, swap = self:get_selection_idx(1, sort)
  end
  return line1, col1, line2, col2, swap
end

---Get the selection specified by `idx`
---@param idx integer @the index of the selection to retrieve
---@param sort? boolean @whether to sort the selection returned
---@return integer,integer,integer,integer,boolean? @line1, col1, line2, col2, was the selection sorted
function Doc:get_selection_idx(idx, sort)
  local line1, col1, line2, col2 = self.selections[idx * 4 - 3], self.selections[idx * 4 - 2],
      self.selections[idx * 4 - 1],
      self.selections[idx * 4]
  if line1 and sort then
    return sort_positions(line1, col1, line2, col2)
  else
    return line1, col1, line2, col2
  end
end

function Doc:get_selection_text(limit)
  limit = limit or math.huge
  local result = {}
  for idx, line1, col1, line2, col2 in self:get_selections() do
    if idx > limit then break end
    if line1 ~= line2 or col1 ~= col2 then
      local text = self:get_text(line1, col1, line2, col2)
      if text ~= "" then result[#result + 1] = text end
    end
  end
  return table.concat(result, "\n")
end

function Doc:has_selection()
  local line1, col1, line2, col2 = self:get_selection(false)
  return line1 ~= line2 or col1 ~= col2
end

function Doc:has_any_selection()
  for idx, line1, col1, line2, col2 in self:get_selections() do
    if line1 ~= line2 or col1 ~= col2 then return true end
  end
  return false
end

function Doc:sanitize_selection()
  for idx, line1, col1, line2, col2 in self:get_selections() do
    self:set_selections(idx, line1, col1, line2, col2)
  end
end

function Doc:set_selections(idx, line1, col1, line2, col2, swap, rm)
  assert(not line2 == not col2, "expected 3 or 5 arguments")
  if swap then line1, col1, line2, col2 = line2, col2, line1, col1 end
  line1, col1 = self:sanitize_position(line1, col1)
  line2, col2 = self:sanitize_position(line2 or line1, col2 or col1)
  common.splice(self.selections, (idx - 1) * 4 + 1, rm == nil and 4 or rm, { line1, col1, line2, col2 })
  sync_native_selections(self)
end

function Doc:add_selection(line1, col1, line2, col2, swap)
  local l1, c1 = sort_positions(line1, col1, line2 or line1, col2 or col1)
  local target = #self.selections / 4 + 1
  for idx, tl1, tc1 in self:get_selections(true) do
    if l1 < tl1 or l1 == tl1 and c1 < tc1 then
      target = idx
      break
    end
  end
  self:set_selections(target, line1, col1, line2, col2, swap, 0)
  self.last_selection = target
end

function Doc:remove_selection(idx)
  if self.last_selection >= idx then
    self.last_selection = self.last_selection - 1
  end
  common.splice(self.selections, (idx - 1) * 4 + 1, 4)
  sync_native_selections(self)
end

function Doc:set_selection(line1, col1, line2, col2, swap)
  self.selections = {}
  self:set_selections(1, line1, col1, line2, col2, swap)
  self.last_selection = 1
  sync_native_selections(self)
end

function Doc:merge_cursors(idx)
  local table_index = idx and (idx - 1) * 4 + 1
  for i = (table_index or (#self.selections - 3)), (table_index or 5), -4 do
    for j = 1, i - 4, 4 do
      if self.selections[i] == self.selections[j] and
          self.selections[i + 1] == self.selections[j + 1] then
        common.splice(self.selections, i, 4)
        if self.last_selection >= (i + 3) / 4 then
          self.last_selection = self.last_selection - 1
        end
        break
      end
    end
  end
  sync_native_selections(self)
end

local function selection_iterator(invariant, idx)
  local target = invariant[3] and (idx * 4 - 7) or (idx * 4 + 1)
  if target > #invariant[1] or target <= 0 or (type(invariant[3]) == "number" and invariant[3] ~= idx - 1) then return end
  if invariant[2] then
    return idx + (invariant[3] and -1 or 1), sort_positions(table.unpack(invariant[1], target, target + 4))
  else
    return idx + (invariant[3] and -1 or 1), table.unpack(invariant[1], target, target + 4)
  end
end

-- If idx_reverse is true, it'll reverse iterate. If nil, or false, regular iterate.
-- If a number, runs for exactly that iteration.
function Doc:get_selections(sort_intra, idx_reverse)
  return selection_iterator, { self.selections, sort_intra, idx_reverse },
      idx_reverse == true and ((#self.selections / 4) + 1) or ((idx_reverse or -1) + 1)
end

function Doc:sanitize_position(line, col)
  local nlines = #self.lines
  if line > nlines then
    return nlines, #self.lines[nlines]
  elseif line < 1 then
    return 1, 1
  end
  return line, common.clamp(col, 1, #self.lines[line])
end

local function position_offset_func(self, line, col, fn, ...)
  line, col = self:sanitize_position(line, col)
  return fn(self, line, col, ...)
end

local function position_offset_byte(self, line, col, offset)
  return doc_native.buffer_position_offset(self.buffer_id, line, col, offset)
end

local function position_offset_linecol(self, line, col, lineoffset, coloffset)
  return self:sanitize_position(line + lineoffset, col + coloffset)
end

function Doc:position_offset(line, col, ...)
  self:ensure_loaded()
  if type(...) ~= "number" then
    return position_offset_func(self, line, col, ...)
  elseif select("#", ...) == 1 then
    return position_offset_byte(self, line, col, ...)
  elseif select("#", ...) == 2 then
    return position_offset_linecol(self, line, col, ...)
  else
    error("bad number of arguments")
  end
end

---Returns the content of the doc between two positions. </br>
---The positions will be sanitized and sorted. </br>
---The character at the "end" position is not included by default.
---@see core.doc.sanitize_position
---@param line1 integer
---@param col1 integer
---@param line2 integer
---@param col2 integer
---@param inclusive boolean? Whether or not to return the character at the last position
---@return string
function Doc:get_text(line1, col1, line2, col2, inclusive)
  self:ensure_loaded()
  line1, col1 = self:sanitize_position(line1, col1)
  line2, col2 = self:sanitize_position(line2, col2)
  line1, col1, line2, col2 = sort_positions(line1, col1, line2, col2)
  return doc_native.buffer_get_text(self.buffer_id, line1, col1, line2, col2, inclusive)
end

function Doc:get_char(line, col)
  self:ensure_loaded()
  line, col = self:sanitize_position(line, col)
  return self.lines[line]:sub(col, col)
end

local function push_undo(undo_stack, time, type, ...)
  undo_stack[undo_stack.idx] = { type = type, time = time, ... }
  undo_stack[undo_stack.idx - config.max_undos] = nil
  undo_stack.idx = undo_stack.idx + 1
end

local function apply_native_edit_result(self, result, undo_stack, time, line_hint)
  if not result then
    return false
  end
  local old_lines = #self.lines
  apply_native_snapshot(self, result)
  local line_delta = result.line_delta or (#self.lines - old_lines)
  if line_delta > 0 then
    self.highlighter:insert_notify(line_hint, line_delta)
  elseif line_delta < 0 then
    self.highlighter:remove_notify(line_hint, -line_delta)
  else
    self.highlighter:invalidate(line_hint)
  end
  self:sanitize_selection()
  return true
end

local function pop_undo(self, undo_stack, redo_stack, modified)
  local cmd = undo_stack[undo_stack.idx - 1]
  if not cmd then return end
  undo_stack.idx = undo_stack.idx - 1

  if cmd.type == "insert" then
    local line, col, text = table.unpack(cmd)
    self:raw_insert(line, col, text, redo_stack, cmd.time)
  elseif cmd.type == "remove" then
    local line1, col1, line2, col2 = table.unpack(cmd)
    self:raw_remove(line1, col1, line2, col2, redo_stack, cmd.time)
  elseif cmd.type == "selection" then
    self.selections = { table.unpack(cmd) }
    self:sanitize_selection()
  end

  modified = modified or (cmd.type ~= "selection")

  -- if next undo command is within the merge timeout then treat as a single
  -- command and continue to execute it
  local next = undo_stack[undo_stack.idx - 1]
  if next and math.abs(cmd.time - next.time) < config.undo_merge_timeout then
    return pop_undo(self, undo_stack, redo_stack, modified)
  end

  if modified then
    self:on_text_change("undo")
  end
end

function Doc:raw_insert(line, col, text, undo_stack, time)
  sync_native_selections(self)
  local result = doc_native.buffer_apply_insert(self.buffer_id, line, col, text)
  if apply_native_edit_result(self, result, undo_stack, time, line) then
    return
  end
end

function Doc:raw_remove(line1, col1, line2, col2, undo_stack, time)
  sync_native_selections(self)
  local result = doc_native.buffer_apply_remove(self.buffer_id, line1, col1, line2, col2)
  if apply_native_edit_result(self, result, undo_stack, time, line1) then
    return
  end
end

function Doc:insert(line, col, text)
  self:ensure_loaded()
  if self.read_only then
    show_read_only_message(self)
    return
  end
  self.redo_stack = { idx = 1 }
  -- Reset the clean id when we're pushing something new before it
  if self:get_change_id() < self.clean_change_id then
    self.clean_change_id = -1
  end
  line, col = self:sanitize_position(line, col)
  self:raw_insert(line, col, text, self.undo_stack, system.get_time())
  self:on_text_change("insert")
end

function Doc:remove(line1, col1, line2, col2)
  self:ensure_loaded()
  if self.read_only then
    show_read_only_message(self)
    return
  end
  self.redo_stack = { idx = 1 }
  line1, col1 = self:sanitize_position(line1, col1)
  line2, col2 = self:sanitize_position(line2, col2)
  line1, col1, line2, col2 = sort_positions(line1, col1, line2, col2)
  self:raw_remove(line1, col1, line2, col2, self.undo_stack, system.get_time())
  self:on_text_change("remove")
end

function Doc:undo()
  self:ensure_loaded()
  if self.read_only then
    show_read_only_message(self)
    return
  end
  apply_native_snapshot(self, doc_native.buffer_undo(self.buffer_id))
  self.highlighter:soft_reset()
  self:on_text_change("undo")
end

function Doc:redo()
  self:ensure_loaded()
  if self.read_only then
    show_read_only_message(self)
    return
  end
  apply_native_snapshot(self, doc_native.buffer_redo(self.buffer_id))
  self.highlighter:soft_reset()
  self:on_text_change("undo")
end

function Doc:apply_edits(edits)
  self:ensure_loaded()
  if self.read_only then
    show_read_only_message(self)
    return false
  end
  if not edits or #edits == 0 then
    return false
  end
  self.redo_stack = { idx = 1 }
  if self:get_change_id() < self.clean_change_id then
    self.clean_change_id = -1
  end
  sync_native_selections(self)
  local result = doc_native.buffer_apply_edits(self.buffer_id, edits)
  if not apply_native_edit_result(self, result, self.undo_stack, system.get_time(), edits[1].line1 or 1) then
    return false
  end
  self:on_text_change("insert")
  return true
end

function Doc:text_input(text, idx)
  self:ensure_loaded()
  for sidx, line1, col1, line2, col2 in self:get_selections(true, idx or true) do
    local had_selection = false
    if line1 ~= line2 or col1 ~= col2 then
      self:delete_to_cursor(sidx)
      had_selection = true
    end

    if self.overwrite
    and not had_selection
    and col1 < #self.lines[line1]
    and text:ulen() == 1 then
      self:remove(line1, col1, translate.next_char(self, line1, col1))
    end

    self:insert(line1, col1, text)
    self:move_to_cursor(sidx, #text)
  end
end

function Doc:ime_text_editing(text, start, length, idx)
  self:ensure_loaded()
  for sidx, line1, col1, line2, col2 in self:get_selections(true, idx or true) do
    if line1 ~= line2 or col1 ~= col2 then
      self:delete_to_cursor(sidx)
    end
    self:insert(line1, col1, text)
    self:set_selections(sidx, line1, col1 + #text, line1, col1)
  end
end

function Doc:replace_cursor(idx, line1, col1, line2, col2, fn)
  local old_text = self:get_text(line1, col1, line2, col2)
  local new_text, res = fn(old_text)
  if old_text ~= new_text then
    self:insert(line2, col2, new_text)
    self:remove(line1, col1, line2, col2)
    if line1 == line2 and col1 == col2 then
      line2, col2 = self:position_offset(line1, col1, #new_text)
      self:set_selections(idx, line1, col1, line2, col2)
    end
  end
  return res
end

function Doc:replace(fn)
  self:ensure_loaded()
  local has_selection, results = false, {}
  for idx, line1, col1, line2, col2 in self:get_selections(true) do
    if line1 ~= line2 or col1 ~= col2 then
      results[idx] = self:replace_cursor(idx, line1, col1, line2, col2, fn)
      has_selection = true
    end
  end
  if not has_selection then
    self:set_selection(table.unpack(self.selections))
    results[1] = self:replace_cursor(1, 1, 1, #self.lines, #self.lines[#self.lines], fn)
  end
  return results
end

function Doc:delete_to_cursor(idx, ...)
  for sidx, line1, col1, line2, col2 in self:get_selections(true, idx) do
    if line1 ~= line2 or col1 ~= col2 then
      self:remove(line1, col1, line2, col2)
    else
      local l2, c2 = self:position_offset(line1, col1, ...)
      self:remove(line1, col1, l2, c2)
      line1, col1 = sort_positions(line1, col1, l2, c2)
    end
    self:set_selections(sidx, line1, col1)
  end
  self:merge_cursors(idx)
end

function Doc:delete_to(...) return self:delete_to_cursor(nil, ...) end

function Doc:move_to_cursor(idx, ...)
  for sidx, line, col in self:get_selections(false, idx) do
    self:set_selections(sidx, self:position_offset(line, col, ...))
  end
  self:merge_cursors(idx)
end

function Doc:move_to(...) return self:move_to_cursor(nil, ...) end

function Doc:select_to_cursor(idx, ...)
  for sidx, line, col, line2, col2 in self:get_selections(false, idx) do
    line, col = self:position_offset(line, col, ...)
    self:set_selections(sidx, line, col, line2, col2)
  end
  self:merge_cursors(idx)
end

function Doc:select_to(...) return self:select_to_cursor(nil, ...) end

function Doc:get_indent_string()
  local indent_type, indent_size = self:get_indent_info()
  if indent_type == "hard" then
    return "\t"
  end
  return string.rep(" ", indent_size)
end

-- Returns the size of the original indent, and the indent
-- in your config format, rounded either up or down.
function Doc:get_line_indent(line, rnd_up)
  local _, e = line:find("^[ \t]+")
  local indent_type, indent_size = self:get_indent_info()
  local soft_tab = string.rep(" ", indent_size)
  if indent_type == "hard" then
    local indent = e and line:sub(1, e):gsub(soft_tab, "\t") or ""
    return e, indent:gsub(" +", rnd_up and "\t" or "")
  else
    local indent = e and line:sub(1, e):gsub("\t", soft_tab) or ""
    local number = #indent / #soft_tab
    return e, indent:sub(1,
      (rnd_up and math.ceil(number) or math.floor(number)) * #soft_tab)
  end
end

-- Un/indents text; behaviour varies based on selection and un/indent.
-- * if there's a selection, it will stay static around the
--   text for both indenting and unindenting.
-- * if you are in the beginning whitespace of a line, and are indenting, the
--   cursor will insert the exactly appropriate amount of spaces, and jump the
--   cursor to the beginning of first non whitespace characters
-- * if you are not in the beginning whitespace of a line, and you indent, it
--   inserts the appropriate whitespace, as if you typed them normally.
-- * if you are unindenting, the cursor will jump to the start of the line,
--   and remove the appropriate amount of spaces (or a tab).
function Doc:indent_text(unindent, line1, col1, line2, col2)
  local text = self:get_indent_string()
  local _, se = self.lines[line1]:find("^[ \t]+")
  local in_beginning_whitespace = col1 == 1 or (se and col1 <= se + 1)
  local has_selection = line1 ~= line2 or col1 ~= col2
  if unindent or has_selection or in_beginning_whitespace then
    local l1d, l2d = #self.lines[line1], #self.lines[line2]
    for line = line1, line2 do
      if not has_selection or #self.lines[line] > 1 then -- don't indent empty lines in a selection
        local e, rnded = self:get_line_indent(self.lines[line], unindent)
        self:remove(line, 1, line, (e or 0) + 1)
        self:insert(line, 1,
          unindent and rnded:sub(1, #rnded - #text) or rnded .. text)
      end
    end
    l1d, l2d = #self.lines[line1] - l1d, #self.lines[line2] - l2d
    if (unindent or in_beginning_whitespace) and not has_selection then
      local start_cursor = (se and se + 1 or 1) + l1d or #(self.lines[line1])
      return line1, start_cursor, line2, start_cursor
    end
    return line1, col1 + l1d, line2, col2 + l2d
  end
  self:insert(line1, col1, text)
  return line1, col1 + #text, line1, col1 + #text
end

-- For plugins to add custom actions on document change.
function Doc:on_text_change(type)
end

-- For plugins to get notified when a document is closed.
function Doc:on_close()
  if self.buffer_id then
    doc_native.buffer_free(self.buffer_id)
    self.buffer_id = nil
  end
  core.log_quiet("Closed doc \"%s\"", self:get_name())
end

return Doc
"##;

/// Registers "core.doc" as a Rust-owned preload.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let package: LuaTable = lua.globals().get("package")?;
    let preload: LuaTable = package.get("preload")?;
    preload.set(
        "core.doc",
        lua.create_function(|lua, ()| {
            lua.load(BOOTSTRAP)
                .set_name("core.doc")
                .eval::<LuaValue>()
        })?,
    )?;
    Ok(())
}
