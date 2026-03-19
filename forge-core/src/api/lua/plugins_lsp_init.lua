-- mod-version:4
local core = require "core"
local common = require "core.common"
local config = require "core.config"
local keymap = require "core.keymap"
local Doc = require "core.doc"
local DocView = require "core.docview"
local style = require "core.style"

config.plugins.lsp = common.merge({
  config_spec = {
    name = "LSP",
    {
      label = "Load On Startup",
      description = "Load the LSP plugin during editor startup.",
      path = "load_on_startup",
      type = "toggle",
      default = true,
    },
    {
      label = "Semantic Highlighting",
      description = "Apply semantic token overlays from LSP servers.",
      path = "semantic_highlighting",
      type = "toggle",
      default = true,
    },
    {
      label = "Inline Diagnostics",
      description = "Render LSP diagnostics in the editor gutter and text area.",
      path = "inline_diagnostics",
      type = "toggle",
      default = true,
    },
    {
      label = "Format On Save",
      description = "Run document formatting before saving when the server supports it.",
      path = "format_on_save",
      type = "toggle",
      default = true,
    },
  },
  load_on_startup = config.lsp.load_on_startup ~= false,
  semantic_highlighting = config.lsp.semantic_highlighting ~= false,
  inline_diagnostics = config.lsp.inline_diagnostics ~= false,
  format_on_save = config.lsp.format_on_save ~= false,
}, config.plugins.lsp)

local manager = require ".server-manager"

local diagnostic_tooltip_offset = style.font:get_height()
local diagnostic_tooltip_border = 1
local diagnostic_tooltip_max_width = math.floor(420 * SCALE)
local diagnostic_tooltip_delay = 0.18
local inline_diagnostic_gap = math.floor(style.font:get_width("  "))
local inline_diagnostic_side_padding = math.max(style.padding.x, math.floor(style.font:get_width(" ")))
local draw_inline_diagnostic

local function trim_text(text)
  return (tostring(text):gsub("^%s+", ""):gsub("%s+$", ""))
end

manager.reload_config()
manager.start_semantic_refresh_loop()

local old_open_doc = core.open_doc
function core.open_doc(filename, ...)
  local doc = old_open_doc(filename, ...)
  if doc and doc.abs_filename and not doc.large_file_mode then
    manager.open_doc(doc)
  end
  return doc
end

local old_on_text_change = Doc.on_text_change
function Doc:on_text_change(change_type)
  old_on_text_change(self, change_type)
  if self.abs_filename and not self.large_file_mode then
    manager.on_doc_change(self)
  end
end

local RootView = require "core.rootview"
local old_on_text_input = RootView.on_text_input
RootView.on_text_input = function(self, text, ...)
  old_on_text_input(self, text, ...)
  manager.maybe_trigger_completion(text)
  manager.maybe_trigger_signature_help(text)
end

local function diagnostic_color(severity)
  if severity == 1 then
    return style.lint.error or style.error
  elseif severity == 2 then
    return style.lint.warning or style.warn
  elseif severity == 3 then
    return style.lint.info or style.accent
  end
  return style.lint.hint or style.good or style.accent
end

local old_draw_line_gutter = DocView.draw_line_gutter
function DocView:draw_line_gutter(line, x, y, width)
  local lh = old_draw_line_gutter(self, line, x, y, width)
  if config.plugins.lsp.inline_diagnostics == false or not self.doc.abs_filename then
    return lh
  end
  if self.doc.large_file_mode then
    return lh
  end

  local severity = manager.get_line_diagnostic_severity(self.doc, line)
  if severity then
    local marker_size = math.max(4, math.floor(self:get_line_height() * 0.22))
    local marker_x = x + math.max(2, style.padding.x - marker_size - 2)
    local marker_y = y + math.floor((self:get_line_height() - marker_size) / 2)
    renderer.draw_rect(marker_x, marker_y, marker_size, marker_size, diagnostic_color(severity))
    local current_line = select(1, self.doc:get_selection())
    if line == current_line then
      renderer.draw_rect(marker_x + marker_size + 2, marker_y, marker_size, marker_size, style.accent)
    end
  end

  return lh
end

local old_docview_mouse_pressed = DocView.on_mouse_pressed
function DocView:on_mouse_pressed(button, x, y, clicks)
  if button == "left" and self.hovering_gutter and not self.doc.large_file_mode then
    local line = self:resolve_screen_position(x, y)
    if manager.get_line_diagnostic_severity(self.doc, line) then
      local marker_size = math.max(4, math.floor(self:get_line_height() * 0.22))
      local marker_x = self.position.x + math.max(2, style.padding.x - marker_size - 2)
      if x >= marker_x and x <= marker_x + marker_size * 2 + 4 then
        manager.quick_fix_for_line(line)
        return true
      end
    end
  end
  return old_docview_mouse_pressed(self, button, x, y, clicks)
end

local old_draw_overlay = DocView.draw_overlay
function DocView:draw_overlay()
  old_draw_overlay(self)
  if config.plugins.lsp.inline_diagnostics == false or not self.doc.abs_filename then
    return
  end
  if self.doc.large_file_mode then
    return
  end

  local minline, maxline = self:get_visible_line_range()
  local line_size = math.max(1, style.caret_width)
  for line = minline, maxline do
    local segments = manager.get_line_diagnostic_segments(self.doc, line)
    if segments then
      local _, y = self:get_line_screen_position(line)
      local lh = self:get_line_height()
      for i = 1, #segments do
        local segment = segments[i]
        local start_x = self:get_line_screen_position(line, segment.col1)
        local end_x = self:get_line_screen_position(line, segment.col2)
        local width = math.max(math.abs(end_x - start_x), math.max(2, style.caret_width * 2))
        renderer.draw_rect(
          math.min(start_x, end_x),
          y + lh - line_size,
          width,
          line_size,
          diagnostic_color(segment.severity)
        )
      end
    end
    draw_inline_diagnostic(self, line)
  end

  local tooltip = self.lsp_diagnostic_tooltip
  if tooltip and tooltip.text and tooltip.alpha > 0 then
    core.root_view:defer_draw(function(view)
      view:draw_lsp_diagnostic_tooltip()
    end, self)
  end
end

local function diagnostic_tooltip_text(diagnostic)
  if not diagnostic then
    return nil
  end
  local parts = {}
  local severity = diagnostic.severity or 3
  local labels = {
    [1] = "Error",
    [2] = "Warning",
    [3] = "Info",
    [4] = "Hint",
  }
  parts[#parts + 1] = labels[severity] or "Diagnostic"
  if diagnostic.source and diagnostic.source ~= "" then
    parts[#parts + 1] = tostring(diagnostic.source)
  end
  if diagnostic.code ~= nil and tostring(diagnostic.code) ~= "" then
    parts[#parts + 1] = tostring(diagnostic.code)
  end

  local prefix = table.concat(parts, " · ")
  local message = tostring(diagnostic.message or ""):gsub("\r\n", "\n"):gsub("\r", "\n")
  if prefix ~= "" then
    return prefix .. "\n" .. message
  end
  return message
end

local function wrap_tooltip_lines(font, text, max_width)
  local lines = {}
  for raw_line in tostring(text or ""):gmatch("([^\n]*)\n?") do
    if raw_line == "" and #lines > 0 and lines[#lines] == "" then
      break
    end
    local remaining = raw_line
    if remaining == "" then
      lines[#lines + 1] = ""
    end
    while remaining ~= "" do
      local candidate = remaining
      if font:get_width(candidate) <= max_width then
        lines[#lines + 1] = candidate
        break
      end
      local cut = #candidate
      while cut > 1 and font:get_width(candidate:sub(1, cut)) > max_width do
        cut = cut - 1
      end
      local split = candidate:sub(1, cut):match("^.*()%s+")
      if split and split > 1 then
        cut = split
      end
      local line = trim_text(candidate:sub(1, cut))
      if line == "" then
        line = candidate:sub(1, math.max(1, cut))
      end
      lines[#lines + 1] = line
      remaining = trim_text(candidate:sub(cut + 1))
    end
  end
  return lines
end

local function inline_diagnostic_text(diagnostic)
  if not diagnostic then
    return nil
  end
  local message = tostring(diagnostic.message or ""):gsub("\r\n", "\n"):gsub("\r", "\n")
  local first_line = trim_text((message:match("([^\n]+)") or ""))
  if first_line == "" then
    return nil
  end
  return first_line:gsub("%s+", " ")
end

draw_inline_diagnostic = function(view, line)
  local diagnostic, end_col = manager.get_inline_diagnostic(view.doc, line)
  local text = inline_diagnostic_text(diagnostic)
  if not text then
    return
  end

  local font = view:get_font()
  local text_w = font:get_width(text)
  if text_w <= 0 then
    return
  end

  local x, y = view:get_line_screen_position(line)
  local lh = view:get_line_height()
  local _, _, scroll_w = view.v_scrollbar:get_track_rect()
  local clip_left = view.position.x + view:get_gutter_width()
  local clip_right = view.position.x + view.size.x - scroll_w
  local max_x = clip_right - inline_diagnostic_side_padding - text_w
  if max_x <= clip_left then
    return
  end

  local line_text = view.doc.lines[line] or "\n"
  local anchor_col = common.clamp((end_col or (#line_text + 1)) + 1, 1, #line_text + 1)
  local anchor_x = x + view:get_col_x_offset(line, anchor_col) + inline_diagnostic_gap
  local text_x = math.max(anchor_x, max_x)
  if text_x + text_w > clip_right - inline_diagnostic_side_padding then
    return
  end

  renderer.draw_rect(
    text_x - inline_diagnostic_side_padding,
    y,
    text_w + inline_diagnostic_side_padding * 2,
    lh,
    style.background
  )
  common.draw_text(
    font,
    diagnostic_color(diagnostic.severity or 3),
    text,
    nil,
    text_x,
    y + view:get_line_text_y_offset(),
    text_w,
    font:get_height()
  )
end

function DocView:update_lsp_diagnostic_tooltip(x, y)
  if config.plugins.lsp.inline_diagnostics == false or not self.doc.abs_filename or self.doc.large_file_mode then
    self.lsp_diagnostic_tooltip = nil
    return
  end

  local tooltip = self.lsp_diagnostic_tooltip or { x = 0, y = 0, begin = 0, alpha = 0 }
  local line, col = self:resolve_screen_position(x, y)
  local diagnostic = nil
  if self.hovering_gutter then
    diagnostic = manager.get_hover_diagnostic(self.doc, line, nil)
  else
    diagnostic = manager.get_hover_diagnostic(self.doc, line, col)
  end

  local text = diagnostic_tooltip_text(diagnostic)
  if text then
    if tooltip.text ~= text then
      tooltip.text = text
      tooltip.lines = wrap_tooltip_lines(style.font, text, diagnostic_tooltip_max_width - style.padding.x * 2)
      tooltip.begin = system.get_time()
      tooltip.alpha = 0
    end
    tooltip.x = x
    tooltip.y = y
    self.lsp_diagnostic_tooltip = tooltip
    if system.get_time() - tooltip.begin > diagnostic_tooltip_delay then
      self:move_towards(tooltip, "alpha", 255, 1, "lsp_diagnostic_tooltip")
    else
      tooltip.alpha = 0
    end
  else
    self.lsp_diagnostic_tooltip = nil
  end
end

function DocView:draw_lsp_diagnostic_tooltip()
  local tooltip = self.lsp_diagnostic_tooltip
  if not (tooltip and tooltip.text and tooltip.alpha > 0) then
    return
  end

  local lines = tooltip.lines or { tooltip.text }
  local line_height = style.font:get_height()
  local text_w = 0
  for i = 1, #lines do
    text_w = math.max(text_w, style.font:get_width(lines[i]))
  end
  local w = math.min(diagnostic_tooltip_max_width, text_w + style.padding.x * 2)
  local h = math.max(line_height, #lines * line_height) + style.padding.y * 2
  local x = tooltip.x + diagnostic_tooltip_offset
  local y = tooltip.y + diagnostic_tooltip_offset
  local root_w = core.root_view.root_node.size.x
  local root_h = core.root_view.root_node.size.y

  if x + w > root_w - style.padding.x then
    x = tooltip.x - w - diagnostic_tooltip_offset
  end
  if x < style.padding.x then
    x = style.padding.x
  end
  if y + h > root_h - style.padding.y then
    y = tooltip.y - h - diagnostic_tooltip_offset
  end
  if y < style.padding.y then
    y = style.padding.y
  end

  renderer.draw_rect(
    x - diagnostic_tooltip_border,
    y - diagnostic_tooltip_border,
    w + diagnostic_tooltip_border * 2,
    h + diagnostic_tooltip_border * 2,
    { style.text[1], style.text[2], style.text[3], tooltip.alpha }
  )
  renderer.draw_rect(
    x,
    y,
    w,
    h,
    { style.background2[1], style.background2[2], style.background2[3], tooltip.alpha }
  )

  local text_color = { style.text[1], style.text[2], style.text[3], tooltip.alpha }
  for i = 1, #lines do
    common.draw_text(
      style.font,
      text_color,
      lines[i],
      nil,
      x + style.padding.x,
      y + style.padding.y + (i - 1) * line_height,
      w - style.padding.x * 2,
      line_height
    )
  end
end

local old_docview_mouse_moved = DocView.on_mouse_moved
function DocView:on_mouse_moved(x, y, dx, dy)
  old_docview_mouse_moved(self, x, y, dx, dy)
  self:update_lsp_diagnostic_tooltip(x, y)
end

local old_docview_mouse_left = DocView.on_mouse_left
function DocView:on_mouse_left()
  self.lsp_diagnostic_tooltip = nil
  old_docview_mouse_left(self)
end

local old_on_close = Doc.on_close
function Doc:on_close()
  if not self.large_file_mode then
    manager.on_doc_close(self)
  end
  old_on_close(self)
end

local old_save = Doc.save
function Doc:save(...)
  local args = table.pack(...)
  if config.plugins.lsp.format_on_save ~= false
     and not self.large_file_mode
     and not self._formatting_before_save
     and self.abs_filename then
    self._formatting_before_save = true
    manager.format_document_for(self, function()
      local ok, err = pcall(function()
        local result = table.pack(old_save(self, table.unpack(args, 1, args.n)))
        if not self.large_file_mode then
          manager.on_doc_save(self)
        end
        return table.unpack(result, 1, result.n)
      end)
      self._formatting_before_save = false
      if not ok then
        core.error(err)
      end
    end)
    return
  end
  local result = table.pack(old_save(self, table.unpack(args, 1, args.n)))
  if not self.large_file_mode then
    local ok, err = pcall(manager.on_doc_save, self)
    if not ok then
      core.error("Post-save LSP hook failed for %s: %s", self:get_name(), err)
    end
  end
  return table.unpack(result, 1, result.n)
end

for _, doc in ipairs(core.docs) do
  if doc.abs_filename and not doc.large_file_mode then
    manager.open_doc(doc)
  end
end

core.status_view:add_item({
  predicate = function()
    local view = core.active_view
    return view and view:is(DocView) and view.doc and view.doc.abs_filename and not view.doc.large_file_mode
  end,
  name = "lsp:quick-fix",
  alignment = core.status_view.Item.RIGHT,
  get_item = function()
    local view = core.active_view
    local line = select(1, view.doc:get_selection())
    local severity = manager.get_line_diagnostic_severity(view.doc, line)
    if not severity then
      return {}
    end
    return {
      style.accent, style.icon_font, "!",
      style.text, " Quick Fix"
    }
  end,
  command = "lsp:quick-fix",
  tooltip = "Show quick fixes for the current diagnostic line",
})

keymap.add {
  ["ctrl+space"] = "lsp:complete",
  ["f12"] = "lsp:goto-definition",
  ["ctrl+alt+left"] = "lsp:jump-back",
  ["ctrl+f12"] = "lsp:goto-type-definition",
  ["shift+f12"] = "lsp:find-references",
  ["f8"] = "lsp:next-diagnostic",
  ["shift+f8"] = "lsp:previous-diagnostic",
  ["ctrl+t"] = "lsp:show-document-symbols",
  ["ctrl+alt+t"] = "lsp:workspace-symbols",
  ["ctrl+shift+a"] = "lsp:code-action",
  ["alt+return"] = "lsp:quick-fix",
  ["ctrl+shift+space"] = "lsp:signature-help",
  ["alt+shift+f"] = "lsp:format-document",
  ["f2"] = "lsp:rename-symbol",
  ["ctrl+k"] = "lsp:hover",
}

return manager
