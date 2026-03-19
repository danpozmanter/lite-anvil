local core = require "core"
local common = require "core.common"
local tokenizer = require "core.tokenizer"
local Object = require "core.object"
local native_tokenizer = require "native_tokenizer"


local Highlighter = Object:extend()

local calc_signature
local pair_tokens_to_positioned
local positioned_to_pair_tokens
local clone_positioned
local overlay_positioned

function Highlighter:__tostring() return "Highlighter" end

function Highlighter:new(doc)
  self.doc = doc
  self.running = false
  self:reset()
end


-- init incremental syntax highlighting
function Highlighter:start()
  if self.running then return end
  self.running = true
  core.add_thread(function()
    while self.first_invalid_line <= self.max_wanted_line do
      local max = math.min(self.first_invalid_line + 40, self.max_wanted_line)
      local retokenized_from
      for i = self.first_invalid_line, max do
        local state = (i > 1) and self.lines[i - 1].state
        local line = self.lines[i]
        if line and line.resume and (line.init_state ~= state or line.text ~= self.doc.lines[i]) then
          -- Reset the progress if no longer valid
          line.resume = nil
        end
        if not (line and line.init_state == state and line.text == self.doc.lines[i] and not line.resume) then
          retokenized_from = retokenized_from or i
          self.lines[i] = self:tokenize_line(i, state, line and line.resume)
          if line and line.semantic_tokens then
            self.lines[i].semantic_tokens = clone_positioned(line.semantic_tokens)
            self.lines[i].positioned = overlay_positioned(
              self.lines[i].base_positioned,
              self.lines[i].semantic_tokens
            )
            self.lines[i].tokens = positioned_to_pair_tokens(self.lines[i].positioned, self.lines[i].text)
            self.lines[i].signature = calc_signature(self.lines[i].positioned)
          end
          if self.lines[i].resume then
            self.first_invalid_line = i
            goto yield
          end
        elseif retokenized_from then
          self:update_notify(retokenized_from, i - retokenized_from - 1)
          retokenized_from = nil
        end
      end

      self.first_invalid_line = max + 1
      ::yield::
      if retokenized_from then
        self:update_notify(retokenized_from, max - retokenized_from)
      end
      core.redraw = true
      coroutine.yield(0)
    end
    self.max_wanted_line = 0
    self.running = false
  end, self)
end

local function set_max_wanted_lines(self, amount)
  self.max_wanted_line = amount
  if self.first_invalid_line <= self.max_wanted_line then
    self:start()
  end
end


function Highlighter:reset()
  self.lines = {}
  self:soft_reset()
end

function Highlighter:soft_reset()
  for i=1,#self.lines do
    self.lines[i] = false
  end
  self.first_invalid_line = 1
  self.max_wanted_line = 0
end

function Highlighter:invalidate(idx)
  self.first_invalid_line = math.min(self.first_invalid_line, idx)
  set_max_wanted_lines(self, math.min(self.max_wanted_line, #self.doc.lines))
end

function Highlighter:insert_notify(line, n)
  self:invalidate(line)
  local blanks = { }
  for i = 1, n do
    blanks[i] = false
  end
  common.splice(self.lines, line, 0, blanks)
end

function Highlighter:remove_notify(line, n)
  self:invalidate(line)
  common.splice(self.lines, line, n)
end

function Highlighter:update_notify(line, n)
  -- plugins can hook here to be notified that lines have been retokenized
end

calc_signature = function(positioned_tokens)
  if not positioned_tokens or #positioned_tokens == 0 then
    return 0
  end

  local hash = 5381
  for i = 1, #positioned_tokens do
    local token = positioned_tokens[i]
    local part = string.format("%s:%d:%d|", token.type, token.pos, token.len)
    for j = 1, #part do
      hash = ((hash * 33) + part:byte(j)) % 2147483647
    end
  end
  return hash
end

pair_tokens_to_positioned = function(tokens)
  local positioned = {}
  local pos = 0
  for i = 1, #tokens, 2 do
    local token_type = tokens[i]
    local text = tokens[i + 1] or ""
    local len = text:ulen() or #text
    positioned[#positioned + 1] = {
      type = token_type,
      pos = pos,
      len = len,
    }
    pos = pos + len
  end
  return positioned
end

positioned_to_pair_tokens = function(positioned, full_text)
  local pair_tokens = {}
  for i = 1, #positioned do
    local token = positioned[i]
    local start_char = token.pos + 1
    local end_char = token.pos + token.len
    local text = full_text:usub(start_char, end_char)
    if text and #text > 0 then
      pair_tokens[#pair_tokens + 1] = token.type
      pair_tokens[#pair_tokens + 1] = text
    end
  end
  return pair_tokens
end

clone_positioned = function(positioned)
  local copy = {}
  for i = 1, #positioned do
    local token = positioned[i]
    copy[i] = {
      type = token.type,
      pos = token.pos,
      len = token.len,
    }
  end
  return copy
end

local function merge_adjacent(positioned)
  local merged = {}
  for i = 1, #positioned do
    local token = positioned[i]
    if token.len > 0 then
      local prev = merged[#merged]
      if prev and prev.type == token.type and prev.pos + prev.len == token.pos then
        prev.len = prev.len + token.len
      else
        merged[#merged + 1] = {
          type = token.type,
          pos = token.pos,
          len = token.len,
        }
      end
    end
  end
  return merged
end

overlay_positioned = function(base_tokens, overlay_tokens)
  if not overlay_tokens or #overlay_tokens == 0 then
    return clone_positioned(base_tokens)
  end

  local result = {}
  local overlay_idx = 1

  for i = 1, #base_tokens do
    local base = base_tokens[i]
    local cursor = base.pos
    local base_end = base.pos + base.len

    while overlay_idx <= #overlay_tokens and overlay_tokens[overlay_idx].pos + overlay_tokens[overlay_idx].len <= cursor do
      overlay_idx = overlay_idx + 1
    end

    local scan_idx = overlay_idx
    while cursor < base_end do
      local overlay = overlay_tokens[scan_idx]
      if not overlay or overlay.pos >= base_end then
        result[#result + 1] = {
          type = base.type,
          pos = cursor,
          len = base_end - cursor,
        }
        cursor = base_end
      elseif overlay.pos > cursor then
        result[#result + 1] = {
          type = base.type,
          pos = cursor,
          len = overlay.pos - cursor,
        }
        cursor = overlay.pos
      else
        local overlay_end = math.min(base_end, overlay.pos + overlay.len)
        if overlay_end > cursor then
          result[#result + 1] = {
            type = overlay.type,
            pos = cursor,
            len = overlay_end - cursor,
          }
          cursor = overlay_end
        else
          scan_idx = scan_idx + 1
        end
      end
    end
  end

  return merge_adjacent(result)
end


function Highlighter:tokenize_line(idx, state, resume)
  local res = {}
  res.init_state = state
  res.text = self.doc.lines[idx]

  local syntax_name = self.doc.syntax and self.doc.syntax.name
  local native_resume = resume and resume.native_resume or resume
  local ok, tokens, next_state, next_resume = pcall(
    native_tokenizer.tokenize_line,
    syntax_name,
    res.text,
    state,
    native_resume
  )
  if ok then
    res.tokens = tokens
    res.state = next_state
    res.resume = next_resume and { native_resume = next_resume } or nil
  else
    core.error("Native tokenizer error for %s: %s", syntax_name, tokens)
    res.tokens = { "normal", res.text }
    res.state = state or "\0"
  end

  res.base_positioned = pair_tokens_to_positioned(res.tokens)
  res.positioned = clone_positioned(res.base_positioned)
  res.signature = calc_signature(res.positioned)
  return res
end

function Highlighter:merge_line(idx, overlay_tokens)
  local line = self:get_line(idx)
  line.semantic_tokens = overlay_tokens and clone_positioned(overlay_tokens) or nil
  line.positioned = overlay_positioned(line.base_positioned, line.semantic_tokens)
  line.tokens = positioned_to_pair_tokens(line.positioned, line.text)
  line.signature = calc_signature(line.positioned)
  self:update_notify(idx, 0)
end

function Highlighter:get_line_signature(idx)
  local line = self.lines[idx]
  return line and line.signature or 0
end


function Highlighter:get_line(idx)
  local line = self.lines[idx]
  if not line or line.text ~= self.doc.lines[idx] then
    local prev = self.lines[idx - 1]
    local old_line = line
    line = self:tokenize_line(idx, prev and prev.state)
    if old_line and old_line.semantic_tokens then
      line.semantic_tokens = clone_positioned(old_line.semantic_tokens)
      line.positioned = overlay_positioned(line.base_positioned, line.semantic_tokens)
      line.tokens = positioned_to_pair_tokens(line.positioned, line.text)
      line.signature = calc_signature(line.positioned)
    end
    self.lines[idx] = line
    self:update_notify(idx, 0)
  end
  set_max_wanted_lines(self, math.max(self.max_wanted_line, idx))
  return line
end


function Highlighter:each_token(idx)
  return tokenizer.each_token(self:get_line(idx).tokens)
end

return Highlighter
