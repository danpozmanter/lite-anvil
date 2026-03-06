use mlua::prelude::*;

// Lua 5.4 source for the utf8extra module.
//
// Pattern-matching functions delegate to the Lua standard string library
// (which works correctly for the ASCII-dominated patterns used in the editor).
// All other functions implement the luautf8 API subset used by utf8string.lua.
const LUA_SRC: &str = r#"
local m = {}

-- Pattern functions: delegate to string library (byte-oriented, works for
-- ASCII patterns which cover all editor usage of find/match/gmatch/gsub).
m.find   = string.find
m.match  = string.match
m.gmatch = string.gmatch
m.gsub   = string.gsub
m.byte   = string.byte

-- From Lua 5.4 built-in utf8 library.
m.char      = utf8.char
m.codepoint = utf8.codepoint
m.codes     = utf8.codes
m.offset    = utf8.offset

-- charpattern constant.
m.charpattern = utf8.charpattern

-- ── len(s [, i [, j]]) ────────────────────────────────────────────────────
-- Returns the number of UTF-8 characters in s[i..j] (1-based byte indices).
function m.len(s, i, j)
  local len = #s
  i = i or 1
  j = j or len
  if i < 0 then i = len + i + 1 end
  if j < 0 then j = len + j + 1 end
  if i < 1 then i = 1 end
  if j > len then j = len end
  if i > j then return 0 end
  local count = 0
  for k = i, j do
    local b = string.byte(s, k)
    if b and (b & 0xC0) ~= 0x80 then
      count = count + 1
    end
  end
  return count
end

-- ── charpos(s, n) ─────────────────────────────────────────────────────────
-- Returns the 1-based byte position of the n-th UTF-8 character (1-based).
-- Returns nil if n is out of range.
function m.charpos(s, n)
  if n == 0 then return 1 end
  local count = 0
  local slen = #s
  for i = 1, slen do
    local b = string.byte(s, i)
    if (b & 0xC0) ~= 0x80 then
      count = count + 1
      if count == n then return i end
    end
  end
  return nil
end

-- ── sub(s, i [, j]) ───────────────────────────────────────────────────────
-- Returns substring by 1-based character indices (negative = from end).
function m.sub(s, i, j)
  local nchars = m.len(s)
  if i < 0 then i = nchars + i + 1 end
  if j then
    if j < 0 then j = nchars + j + 1 end
  else
    j = nchars
  end
  if i < 1 then i = 1 end
  if j > nchars then j = nchars end
  if i > j then return "" end
  local bi = m.charpos(s, i)
  if not bi then return "" end
  -- Find end byte: start of char j+1 minus 1, or end of string.
  local bj_next = m.charpos(s, j + 1)
  if bj_next then
    return string.sub(s, bi, bj_next - 1)
  else
    return string.sub(s, bi)
  end
end

-- ── reverse(s) ────────────────────────────────────────────────────────────
function m.reverse(s)
  local chars = {}
  local pos = 1
  local slen = #s
  while pos <= slen do
    local b = string.byte(s, pos)
    local clen = b < 0x80 and 1 or b < 0xE0 and 2 or b < 0xF0 and 3 or 4
    table.insert(chars, string.sub(s, pos, pos + clen - 1))
    pos = pos + clen
  end
  local rev = {}
  for k = #chars, 1, -1 do rev[#rev + 1] = chars[k] end
  return table.concat(rev)
end

-- ── lower / upper ─────────────────────────────────────────────────────────
function m.lower(s) return s:lower() end
function m.upper(s) return s:upper() end

-- ── title(s) ──────────────────────────────────────────────────────────────
function m.title(s)
  if #s == 0 then return s end
  return s:sub(1, 1):upper() .. s:sub(2):lower()
end

-- ── fold(s) ───────────────────────────────────────────────────────────────
function m.fold(s) return s:lower() end

-- ── ncasecmp(s1, s2) ──────────────────────────────────────────────────────
function m.ncasecmp(s1, s2)
  local l1, l2 = s1:lower(), s2:lower()
  if l1 < l2 then return -1 elseif l1 > l2 then return 1 else return 0 end
end

-- ── next(s [, pos]) ───────────────────────────────────────────────────────
-- Returns (end_byte_pos, codepoint) of the character after byte pos (0-based
-- like luautf8).  Returns nil at end of string.
function m.next(s, pos)
  pos = (pos or 0) + 1
  if pos > #s then return nil end
  local b = string.byte(s, pos)
  local clen = b < 0x80 and 1 or b < 0xE0 and 2 or b < 0xF0 and 3 or 4
  -- Decode codepoint.
  local cp
  if clen == 1 then
    cp = b
  elseif clen == 2 then
    local b2 = string.byte(s, pos + 1)
    cp = (b & 0x1F) << 6 | (b2 & 0x3F)
  elseif clen == 3 then
    local b2, b3 = string.byte(s, pos + 1), string.byte(s, pos + 2)
    cp = (b & 0x0F) << 12 | (b2 & 0x3F) << 6 | (b3 & 0x3F)
  else
    local b2, b3, b4 = string.byte(s, pos+1), string.byte(s, pos+2), string.byte(s, pos+3)
    cp = (b & 0x07) << 18 | (b2 & 0x3F) << 12 | (b3 & 0x3F) << 6 | (b4 & 0x3F)
  end
  return pos + clen - 1, cp
end

-- ── escape(s) ─────────────────────────────────────────────────────────────
-- Convert \{XXXX} escape sequences to UTF-8 chars; pass the rest through.
function m.escape(s)
  return (s:gsub("\\{(%x+)}", function(hex)
    return utf8.char(tonumber(hex, 16))
  end))
end

-- ── insert(s, offset, val) / insert(s, val) ───────────────────────────────
function m.insert(s, offset, val)
  if type(offset) == "string" then
    -- 2-arg form: append val after s
    return s .. offset
  end
  local bi = m.charpos(s, offset)
  if not bi then return s .. val end
  return string.sub(s, 1, bi - 1) .. val .. string.sub(s, bi)
end

-- ── remove(s, start [, fin]) ──────────────────────────────────────────────
function m.remove(s, start, fin)
  fin = fin or start
  local bi = m.charpos(s, start)
  local bj_next = m.charpos(s, fin + 1)
  if not bi then return s end
  if bj_next then
    return string.sub(s, 1, bi - 1) .. string.sub(s, bj_next)
  else
    return string.sub(s, 1, bi - 1)
  end
end

-- ── width / widthindex ────────────────────────────────────────────────────
-- Simplified: treat every character as width 1.
function m.width(s, ambi, i)
  return m.len(s)
end

function m.widthindex(s, w, ambi, i)
  return m.charpos(s, w), w
end

return m
"#;

pub fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    lua.load(LUA_SRC).eval()
}
