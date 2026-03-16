-- mod-version:4
local core      = require "core"
local common    = require "core.common"
local config    = require "core.config"
local command   = require "core.command"
local style     = require "core.style"
local keymap    = require "core.keymap"
local translate = require "core.doc.translate"
local RootView  = require "core.rootview"
local DocView   = require "core.docview"
local Doc       = require "core.doc"
local drawing   = require "plugins.autocomplete.drawing"
local symbol_index = nil
local user_autocomplete_config = config.plugins.autocomplete or {}

do
  local ok, mod = pcall(require, "symbol_index")
  if ok then
    symbol_index = mod
  end
end

---Symbols cache of all open documents
---@type table<core.doc, table>
local cache = setmetatable({}, { __mode = "k" })
local next_doc_id = 1

local function get_doc_id(doc)
  local c = cache[doc]
  if c and c.doc_id then
    return c.doc_id
  end
  c = c or {}
  c.doc_id = next_doc_id
  next_doc_id = next_doc_id + 1
  cache[doc] = c
  return c.doc_id
end

config.plugins.autocomplete = common.merge({
  -- Source mode for autocomplete suggestions.
  mode = user_autocomplete_config.mode or "lsp",
  -- Amount of characters that need to be written for autocomplete
  min_len = 3,
  -- The max amount of visible items
  max_height = 6,
  -- The max amount of scrollable items
  max_suggestions = 100,
  -- Maximum amount of symbols to cache per document
  max_symbols = 4000,
  -- Which symbols to show on the suggestions list: global, local, related, none
  suggestions_scope = "global",
  -- Font size of the description box
  desc_font_size = 12,
  -- Do not show the icons associated to the suggestions
  hide_icons = false,
  -- Position where icons will be displayed on the suggestions list
  icon_position = "left",
  -- Do not show the additional information related to a suggestion
  hide_info = false,
  -- The config specification used by gui generators
  config_spec = {
    name = "Autocomplete",
    {
      label = "Mode",
      description = "Choose whether autocomplete uses document symbols, LSP, all known symbols, or stays off.",
      path = "mode",
      type = "selection",
      default = user_autocomplete_config.mode or "lsp",
      values = {
        {"Off", "off"},
        {"In Document", "in_document"},
        {"Via LSP", "lsp"},
        {"Totally On", "totally_on"}
      }
    },
    {
      label = "Minimum Length",
      description = "Amount of characters that need to be written for autocomplete to popup.",
      path = "min_len",
      type = "number",
      default = 3,
      min = 1,
      max = 5
    },
    {
      label = "Maximum Height",
      description = "The maximum amount of visible items.",
      path = "max_height",
      type = "number",
      default = 6,
      min = 1,
      max = 20
    },
    {
      label = "Maximum Suggestions",
      description = "The maximum amount of scrollable items.",
      path = "max_suggestions",
      type = "number",
      default = 100,
      min = 10,
      max = 10000
    },
    {
      label = "Maximum Symbols",
      description = "Maximum amount of symbols to cache per document.",
      path = "max_symbols",
      type = "number",
      default = 4000,
      min = 1000,
      max = 10000
    },
    {
      label = "Description Font Size",
      description = "Font size of the description box.",
      path = "desc_font_size",
      type = "number",
      default = 12,
      min = 8
    },
    {
      label = "Hide Icons",
      description = "Do not show icons on the suggestions list.",
      path = "hide_icons",
      type = "toggle",
      default = false
    },
    {
      label = "Icons Position",
      description = "Position to display icons on the suggestions list.",
      path = "icon_position",
      type = "selection",
      default = "left",
      values = {
        {"Left", "left"},
        {"Right", "Right"}
      }
    },
    {
      label = "Hide Items Info",
      description = "Do not show the additional info related to each suggestion.",
      path = "hide_info",
      type = "toggle",
      default = false
    }
  }
}, config.plugins.autocomplete)

local autocomplete = {}

autocomplete.map = {}
autocomplete.map_manually = {}
autocomplete.on_close = nil
autocomplete.icons = {}
autocomplete.providers = {}

-- Flag that indicates if the autocomplete box was manually triggered
-- with the autocomplete.complete() function to prevent the suggestions
-- from getting cluttered with arbitrary document symbols by using the
-- autocomplete.map_manually table.
local triggered_manually = false
local mode_explicitly_set = user_autocomplete_config.mode ~= nil
local provider_items = nil
local provider_request_id = 0

local mt = { __tostring = function(t) return t.text end }

local function items_from_completion_map(t)
  local items = {}
  for text, info in pairs(t.items) do
    text = text ~= nil and tostring(text) or nil
    if text then
    if type(info) == "table" then
      table.insert(
        items,
        setmetatable(
          {
            text     = text,
            info     = info.info ~= nil and tostring(info.info) or nil,
            icon     = info.icon,         -- Name of icon to show
            desc     = info.desc ~= nil and tostring(info.desc) or nil, -- Description shown on item selected
            onhover  = info.onhover,      -- A callback called once when item is hovered
            onselect = info.onselect,     -- A callback called when item is selected
            data     = info.data          -- Optional data that can be used on cb
          },
          mt
        )
      )
    else
      info = info ~= nil and tostring(info) or nil
      table.insert(items, setmetatable({ text = text, info = info }, mt))
    end
    end
  end
  return items
end

local function current_mode()
  return config.plugins.autocomplete.mode or "off"
end

local function suggestion_scope()
  local mode = current_mode()
  if mode == "in_document" then
    return "local"
  elseif mode == "totally_on" then
    return "global"
  end
  return nil
end

local function provider_for_mode()
  return autocomplete.providers[current_mode()]
end

function autocomplete.add(t, manually_triggered)
  local items = items_from_completion_map(t)

  if not manually_triggered then
    autocomplete.map[t.name] = { files = t.files or ".*", items = items }
  else
    autocomplete.map_manually[t.name] = { files = t.files or ".*", items = items }
  end
end

--
-- Thread that scans open document symbols and caches them
--
local global_symbols = {}

core.add_thread(function()
  local function load_syntax_symbols(doc)
    if doc.syntax and not autocomplete.map["language_" .. doc.syntax.name] then
      local symbols = {
        name  = "language_" .. doc.syntax.name,
        files = doc.syntax.files,
        items = {}
      }
      for name, type in pairs(doc.syntax.symbols) do
        symbols.items[name] = type
      end
      autocomplete.add(symbols)
      return symbols.items
    end
    return {}
  end

  local function get_symbols(doc)
    if doc.large_file_mode and config.large_file.disable_autocomplete ~= false then
      doc.disable_symbols = true
      return {}
    end
    if symbol_index and config.symbol_pattern == "[%a_][%w_]*" then
      local state = symbol_index.set_doc_symbols(
        get_doc_id(doc),
        doc.lines,
        config.plugins.autocomplete.max_symbols,
        load_syntax_symbols(doc)
      )
      if state.exceeded then
        doc.disable_symbols = true
        local filename_message = doc.filename and ("document " .. doc.filename) or "unnamed document"
        core.status_view:show_message("!", style.accent,
          "Too many symbols in " .. filename_message ..
          ": stopping auto-complete for this document according to " ..
          "config.plugins.autocomplete.max_symbols."
        )
        collectgarbage("collect")
        return {}
      end
      return true
    end

    local s = {}
    local syntax_symbols = load_syntax_symbols(doc)
    local max_symbols = config.plugins.autocomplete.max_symbols
    if doc.disable_symbols then return s end
    local i = 1
    local symbols_count = 0
    while i <= #doc.lines do
      for sym in doc.lines[i]:gmatch(config.symbol_pattern) do
        if not s[sym] and not syntax_symbols[sym] then
          symbols_count = symbols_count + 1
          if symbols_count > max_symbols then
            s = nil
            doc.disable_symbols = true
            local filename_message
            if doc.filename then
              filename_message = "document " .. doc.filename
            else
              filename_message = "unnamed document"
            end
            core.status_view:show_message("!", style.accent,
              "Too many symbols in " .. filename_message ..
              ": stopping auto-complete for this document according to " ..
              "config.plugins.autocomplete.max_symbols."
            )
            collectgarbage('collect')
            return {}
          end
          s[sym] = true
        end
      end
      i = i + 1
      if i % 100 == 0 then coroutine.yield() end
    end
    return s
  end

  local function cache_is_valid(doc)
    local c = cache[doc]
    return c and c.last_change_id == doc:get_change_id()
  end

  while true do
    local symbols = {}

    -- lift all symbols from all docs
    for _, doc in ipairs(core.docs) do
      -- update the cache if the doc has changed since the last iteration
      if not cache_is_valid(doc) then
        cache[doc] = {
          doc_id = get_doc_id(doc),
          last_change_id = doc:get_change_id(),
          symbols = get_symbols(doc)
        }
      end
      -- update symbol set with doc's symbol set
      if suggestion_scope() == "global" then
        if symbol_index and cache[doc].symbols == true then
          for _, sym in ipairs(symbol_index.get_doc_symbols(cache[doc].doc_id)) do
            symbols[sym] = true
          end
        else
          for sym in pairs(cache[doc].symbols) do
            symbols[sym] = true
          end
        end
      end
      coroutine.yield()
    end

    -- update global symbols list
    if suggestion_scope() == "global" then
      global_symbols = symbols
    end

    -- wait for next scan
    local valid = true
    while valid do
      coroutine.yield(1)
      for _, doc in ipairs(core.docs) do
        if not cache_is_valid(doc) then
          valid = false
          break
        end
      end
    end
  end
end)


local partial = ""
local suggestions_offset = 1
local suggestions_idx = 1
local suggestions = {}
local last_line, last_col


local function reset_suggestions()
  suggestions_offset = 1
  suggestions_idx = 1
  suggestions = {}
  provider_items = nil
  provider_request_id = provider_request_id + 1

  triggered_manually = false

  local doc = core.active_view.doc
  if autocomplete.on_close then
    autocomplete.on_close(doc, suggestions[suggestions_idx])
    autocomplete.on_close = nil
  end
end

local function update_suggestions()
  local doc = core.active_view.doc
  local filename = doc and doc.filename or ""

  local map = triggered_manually and autocomplete.map_manually or autocomplete.map

  local assigned_sym = {}

  -- get all relevant suggestions for given filename
  local items = {}
  if provider_items then
    for _, item in ipairs(provider_items) do
      table.insert(items, item)
      assigned_sym[item.text] = true
    end
  else
    for _, v in pairs(map) do
      if common.match_pattern(filename, v.files) then
        for _, item in pairs(v.items) do
          table.insert(items, item)
          assigned_sym[item.text] = true
        end
      end
    end
  end

  -- Append the global, local or related text symbols if applicable
  local scope = suggestion_scope()

  if not triggered_manually and not provider_items and scope then
    local text_symbols = nil

    if scope == "global" then
      if symbol_index then
        local doc_ids = {}
        for _, d in ipairs(core.docs) do
          if cache[d] and cache[d].symbols == true then
            doc_ids[#doc_ids + 1] = cache[d].doc_id
          end
        end
        text_symbols = symbol_index.collect(doc_ids)
      else
        text_symbols = global_symbols
      end
    elseif scope == "local" and cache[doc] and cache[doc].symbols then
      if symbol_index and cache[doc].symbols == true then
        text_symbols = symbol_index.get_doc_symbols(cache[doc].doc_id)
      else
        text_symbols = cache[doc].symbols
      end
    elseif scope == "related" then
      for _, d in ipairs(core.docs) do
        if doc.syntax == d.syntax then
          if cache[d] and cache[d].symbols then
            if symbol_index and cache[d].symbols == true then
              for _, name in ipairs(symbol_index.get_doc_symbols(cache[d].doc_id)) do
                if not assigned_sym[name] then
                  table.insert(items, setmetatable({ text = name, info = "normal" }, mt))
                end
              end
            else
              for name in pairs(cache[d].symbols) do
                if not assigned_sym[name] then
                  table.insert(items, setmetatable({ text = name, info = "normal" }, mt))
                end
              end
            end
          end
        end
      end
    end

    if text_symbols then
      if symbol_index and type(text_symbols[1]) ~= "nil" then
        for _, name in ipairs(text_symbols) do
          if not assigned_sym[name] then
            table.insert(items, setmetatable({ text = name, info = "normal" }, mt))
          end
        end
      else
        for name in pairs(text_symbols) do
          if not assigned_sym[name] then
            table.insert(items, setmetatable({ text = name, info = "normal" }, mt))
          end
        end
      end
    end
  end

  -- fuzzy match, remove duplicates and store
  items = common.fuzzy_match(items, partial)
  local j = 1
  for i = 1, config.plugins.autocomplete.max_suggestions do
    suggestions[i] = items[j]
    while items[j] and suggestions[i] and suggestions[i].text == items[j].text do
      suggestions[i].info = suggestions[i].info or items[j].info
      j = j + 1
    end
  end
  suggestions_idx = 1
  suggestions_offset = 1
end

local function get_partial_symbol()
  local doc = core.active_view.doc
  local line2, col2 = doc:get_selection()
  local line1, col1 = doc:position_offset(line2, col2, translate.start_of_word)
  return doc:get_text(line1, col1, line2, col2)
end

local function get_active_view()
  if core.active_view:is(DocView) then
    return core.active_view
  end
end

-- Builds the context table consumed by drawing functions.
local function make_ctx()
  return {
    suggestions        = suggestions,
    suggestions_idx    = suggestions_idx,
    suggestions_offset = suggestions_offset,
    partial            = partial,
    icons              = autocomplete.icons,
  }
end

local function show_autocomplete()
  local av = get_active_view()
  if av then
    if not triggered_manually and current_mode() == "off" then
      reset_suggestions()
      return
    end

    -- update partial symbol and suggestions
    partial = get_partial_symbol()

    if #partial >= config.plugins.autocomplete.min_len or triggered_manually then
      local provider = not triggered_manually and provider_for_mode() or nil
      if provider then
        provider_items = nil
        provider_request_id = provider_request_id + 1
        local request_id = provider_request_id
        local line, col = av.doc:get_selection()
        local doc = av.doc
        local change_id = doc:get_change_id()
        local request_partial = partial
        local function respond(completions)
          if request_id ~= provider_request_id then
            return
          end
          local active_view = get_active_view()
          if not active_view or active_view.doc ~= doc then
            return
          end
          local current_line, current_col = doc:get_selection()
          if current_line ~= line or current_col ~= col then
            return
          end
          if doc:get_change_id() ~= change_id or get_partial_symbol() ~= request_partial then
            return
          end
          provider_items = completions and items_from_completion_map(completions) or {}
          update_suggestions()
        end
        local completions = provider({
          doc = doc,
          line = line,
          col = col,
          partial = request_partial,
          manually = triggered_manually,
        }, respond)
        if completions ~= nil then
          respond(completions)
        end
      else
        update_suggestions()
      end

      if not triggered_manually then
        last_line, last_col = av.doc:get_selection()
      else
        local line, col = av.doc:get_selection()
        local char = av.doc:get_char(line, col - 1, line, col - 1)
        if char:match("%s") or (char:match("%p") and col ~= last_col) then
          reset_suggestions()
        end
      end
    else
      reset_suggestions()
    end

    -- scroll if rect is out of bounds of view
    local _, y, _, h = drawing.get_suggestions_rect(make_ctx(), av)
    local limit = av.position.y + av.size.y
    if y + h > limit then
      av.scroll.to.y = av.scroll.y + y + h - limit
    end
  end
end

--
-- Patch event logic into RootView and Doc
--
local on_text_input = RootView.on_text_input
local on_text_remove = Doc.remove
local update = RootView.update
local draw = RootView.draw
local old_on_close = Doc.on_close

RootView.on_text_input = function(...)
  on_text_input(...)
  if triggered_manually or current_mode() ~= "lsp" then
    show_autocomplete()
  end
end

Doc.remove = function(self, line1, col1, line2, col2)
  on_text_remove(self, line1, col1, line2, col2)

  if triggered_manually and line1 == line2 then
    if last_col >= col1 then
      reset_suggestions()
    else
      show_autocomplete()
    end
  end
end

RootView.update = function(...)
  update(...)

  local av = get_active_view()
  if av then
    -- reset suggestions if caret was moved
    local line, col = av.doc:get_selection()

    if not triggered_manually then
      if line ~= last_line or col ~= last_col then
        reset_suggestions()
      end
    else
      if line ~= last_line or col < last_col then
        reset_suggestions()
      end
    end
  end
end

RootView.draw = function(...)
  draw(...)

  local av = get_active_view()
  if av then
    -- draw suggestions box after everything else
    core.root_view:defer_draw(drawing.draw_suggestions_box, make_ctx(), av)
  end
end

Doc.on_close = function(self, ...)
  if symbol_index and cache[self] and cache[self].doc_id then
    symbol_index.remove_doc(cache[self].doc_id)
  end
  return old_on_close(self, ...)
end

--
-- Public functions
--
function autocomplete.open(on_close)
  triggered_manually = true

  if on_close then
    autocomplete.on_close = on_close
  end

  local av = get_active_view()
  if av then
    partial = get_partial_symbol()
    last_line, last_col = av.doc:get_selection()
    update_suggestions()
  end
end

function autocomplete.close()
  reset_suggestions()
end

function autocomplete.is_open()
  return #suggestions > 0
end

function autocomplete.complete(completions, on_close)
  reset_suggestions()

  autocomplete.map_manually = {}
  autocomplete.add(completions, true)

  autocomplete.open(on_close)
end

function autocomplete.can_complete()
  if current_mode() == "off" then
    return false
  end
  if #partial >= config.plugins.autocomplete.min_len then
    return true
  end
  return false
end

function autocomplete.register_provider(name, provider)
  autocomplete.providers[name] = provider
end

function autocomplete.set_default_mode(mode)
  if not mode_explicitly_set and current_mode() == "lsp" then
    config.plugins.autocomplete.mode = mode
  end
end

---Register a font icon that can be assigned to completion items.
---@param name string
---@param character string
---@param font? renderer.font
---@param color? string | renderer.color A style.syntax[] name or specific color
function autocomplete.add_icon(name, character, font, color)
  local color_type = type(color)
  assert(
    not color or color_type == "table"
      or (color_type == "string" and style.syntax[color]),
    "invalid icon color given"
  )
  autocomplete.icons[name] = {
    char  = character,
    font  = font or style.code_font,
    color = color or "keyword"
  }
end

--
-- Register built-in syntax symbol type icons
--
for name, _ in pairs(style.syntax) do
  autocomplete.add_icon(name, "M", style.icon_font, name)
end

--
-- Commands
--
local function predicate()
  local active_docview = get_active_view()
  return active_docview and #suggestions > 0, active_docview
end

command.add(predicate, {
  ["autocomplete:complete"] = function(dv)
    local doc  = dv.doc
    local item = suggestions[suggestions_idx]
    local inserted = false
    if item.onselect then
      inserted = item.onselect(suggestions_idx, item)
    end
    if not inserted then
      local current_partial = get_partial_symbol()
      local sz = #current_partial

      for _, line1, col1, line2, _ in doc:get_selections(true) do
        local n    = col1 - 1
        local line = doc.lines[line1]
        for i = 1, sz + 1 do
          local j        = sz - i
          local subline   = line:sub(n - j, n)
          local subpartial = current_partial:sub(i, -1)
          if subpartial == subline then
            doc:remove(line1, col1, line2, n - j)
            break
          end
        end
      end

      doc:text_input(item.text)
    end
    reset_suggestions()
  end,

  ["autocomplete:previous"] = function()
    suggestions_idx = (suggestions_idx - 2) % #suggestions + 1

    local ah = math.min(config.plugins.autocomplete.max_height, #suggestions)
    if suggestions_offset > suggestions_idx then
      suggestions_offset = suggestions_idx
    elseif suggestions_offset + ah < suggestions_idx + 1 then
      suggestions_offset = suggestions_idx - ah + 1
    end
  end,

  ["autocomplete:next"] = function()
    suggestions_idx = (suggestions_idx % #suggestions) + 1

    local ah = math.min(config.plugins.autocomplete.max_height, #suggestions)
    if suggestions_offset + ah < suggestions_idx + 1 then
      suggestions_offset = suggestions_idx - ah + 1
    elseif suggestions_offset > suggestions_idx then
      suggestions_offset = suggestions_idx
    end
  end,

  ["autocomplete:cycle"] = function()
    local newidx = suggestions_idx + 1
    suggestions_idx = newidx > #suggestions and 1 or newidx
  end,

  ["autocomplete:cancel"] = function()
    reset_suggestions()
  end,
})

--
-- Keymaps
--
keymap.add {
  ["return"] = "autocomplete:complete",
  ["keypad enter"] = "autocomplete:complete",
  ["tab"]    = "autocomplete:complete",
  ["up"]     = "autocomplete:previous",
  ["down"]   = "autocomplete:next",
  ["escape"] = "autocomplete:cancel",
}


return autocomplete
