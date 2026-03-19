use mlua::prelude::*;

const INIT_SOURCE: &str = include_str!("lua/plugins_lsp_init.lua");

const CLIENT_SOURCE: &str = r#"local core = require "core"
local native_transport = require "lsp_transport"

local Client = {}
Client.__index = Client

function Client.new(name, spec, root_dir, handlers)
  local ok, transport_id = pcall(native_transport.spawn, spec.command, root_dir, spec.env)
  if not ok or not transport_id then
    return nil, transport_id or "failed to start language server"
  end

  local self = setmetatable({
    name = name,
    spec = spec,
    root_dir = root_dir,
    transport_id = transport_id,
    handlers = handlers or {},
    next_request_id = 0,
    pending = {},
    pre_init_queue = {},
    is_initialized = false,
    is_shutting_down = false,
    capabilities = {},
  }, Client)

  self:start_reader()
  return self
end

function Client:is_running()
  local ok, state = pcall(native_transport.poll, self.transport_id, 0)
  return ok and state and state.running
end

function Client:start_reader()
  core.add_thread(function()
    while true do
      local had_output = false
      local ok, polled = pcall(native_transport.poll, self.transport_id, 64)
      if ok and polled then
        if polled.messages and #polled.messages > 0 then
          had_output = true
          for _, message in ipairs(polled.messages) do
            self:handle_message(message)
          end
        end
        if polled.stderr and #polled.stderr > 0 then
          had_output = true
          for _, line in ipairs(polled.stderr) do
            core.log_quiet("LSP %s stderr: %s", self.name, tostring(line):gsub("%s+$", ""))
          end
        end
        if not polled.running and (not polled.messages or #polled.messages == 0)
            and (not polled.stderr or #polled.stderr == 0) then
          break
        end
      end

      if not had_output then
        coroutine.yield(0.05)
      end
    end

    if self.handlers.on_exit then
      core.try(self.handlers.on_exit, self)
    end
  end)
end

function Client:send(message)
  return pcall(native_transport.send, self.transport_id, message)
end

function Client:queue_or_send(message, bypass_init)
  if not bypass_init and not self.is_initialized then
    self.pre_init_queue[#self.pre_init_queue + 1] = message
    return true
  end
  return self:send(message)
end

function Client:notify(method, params, bypass_init)
  return self:queue_or_send({
    jsonrpc = "2.0",
    method = method,
    params = params,
  }, bypass_init)
end

function Client:request(method, params, callback, bypass_init)
  self.next_request_id = self.next_request_id + 1
  local id = self.next_request_id
  if callback then
    self.pending[id] = callback
  end
  return self:queue_or_send({
    jsonrpc = "2.0",
    id = id,
    method = method,
    params = params,
  }, bypass_init)
end

function Client:flush_pre_init_queue()
  local queued = self.pre_init_queue
  self.pre_init_queue = {}
  for _, message in ipairs(queued) do
    self:send(message)
  end
end

function Client:initialize(params, on_ready)
  self:request("initialize", params, function(result, err)
    if err then
      core.warn("LSP %s initialize failed: %s", self.name, err.message or tostring(err))
      return
    end
    self.capabilities = result and result.capabilities or {}
    self.is_initialized = true
    self:notify("initialized", {}, true)
    self:flush_pre_init_queue()
    if on_ready then
      core.try(on_ready, self, result)
    end
  end, true)
end

function Client:handle_message(message)
  if message.id ~= nil then
    local callback = self.pending[message.id]
    self.pending[message.id] = nil
    if callback then
      core.try(callback, message.result, message.error, message)
    end
    return
  end

  if self.handlers.on_notification and message.method then
    core.try(self.handlers.on_notification, self, message)
  end
end

function Client:shutdown()
  if self.is_shutting_down or not self.transport_id then
    return
  end
  self.is_shutting_down = true
  self:request("shutdown", nil, function()
    self:notify("exit", nil, true)
    native_transport.terminate(self.transport_id)
    native_transport.remove(self.transport_id)
  end, true)
end

return Client
"#;

const JSON_SOURCE: &str = r#"local native = require "lsp_protocol"

local json = {}

function json.encode(value)
  return native.json_encode(value)
end

function json.encode_safe(value)
  return pcall(json.encode, value)
end

function json.decode(text)
  return native.json_decode(text)
end

function json.decode_safe(text)
  return pcall(json.decode, text)
end

return json
"#;

const PROTOCOL_SOURCE: &str = r#"local native = require "lsp_protocol"

local protocol = {}

protocol.completion_kinds = native.completion_kinds

function protocol.encode_message(message)
  return native.encode_message(message)
end

function protocol.decode_messages(buffer)
  return native.decode_messages(buffer)
end

return protocol
"#;

const LSP_MANAGER_SOURCE: &str = r#"local core = require "core"
local command = require "core.command"
local common = require "core.common"
local config = require "core.config"
local DocView = require "core.docview"
local style = require "core.style"

local Client = require "..client"
local json = require "..json"
local protocol = require "..protocol"

local autocomplete = require "plugins.autocomplete"
local make_location_items
local native_lsp = require "lsp_manager"
local native_picker = require "picker"

local manager = {
  config_path = nil,
  config_paths = {},
  raw_config = {},
  specs = {},
  clients = {},
  doc_state = setmetatable({}, { __mode = "k" }),
  diagnostics = {},
  location_history = {},
  semantic_refresh_thread_started = false,
}

local function basename(path)
  return path and path:match("([^/\\]+)$") or path
end

local function join_path(left, right)
  if not left or left == "" then
    return right
  end
  return left .. PATHSEP .. right
end

local function uri_encode(path)
  return (path:gsub("[^%w%-%._~/:]", function(char)
    return string.format("%%%02X", char:byte())
  end))
end

local function path_to_uri(path)
  local normalized = common.normalize_path(path):gsub("\\", "/")
  if normalized:sub(1, 1) ~= "/" then
    normalized = "/" .. normalized
  end
  return "file://" .. uri_encode(normalized)
end

local function uri_to_path(uri)
  if type(uri) ~= "string" then
    return nil
  end
  local path = uri:gsub("^file://", "")
  path = path:gsub("%%(%x%x)", function(hex)
    return string.char(tonumber(hex, 16))
  end)
  if PLATFORM == "Windows" then
    path = path:gsub("^/([A-Za-z]:)", "%1")
  end
  return path
end

local function utf8_char_to_byte(text, character)
  if character <= 0 then
    return 1
  end
  local byte = text:uoffset(character + 1)
  if byte then
    return byte
  end
  return #text
end

local function byte_to_utf8_char(text, column)
  local prefix = text:sub(1, math.max(column - 1, 0))
  return prefix:ulen() or #prefix
end

local function lsp_position_from_doc(doc, line, col)
  local text = doc.lines[line] or "\n"
  return {
    line = line - 1,
    character = byte_to_utf8_char(text, col),
  }
end

local function doc_position_from_lsp(doc, position)
  local line = math.max((position.line or 0) + 1, 1)
  local text = doc.lines[line] or "\n"
  local col = utf8_char_to_byte(text, position.character or 0)
  return doc:sanitize_position(line, col)
end

local function full_document_text(doc)
  return doc:get_text(1, 1, math.huge, math.huge)
end

local function current_docview()
  if core.active_view and core.active_view:is(DocView) then
    return core.active_view
  end
end

local function compare_positioned_token(a, b)
  if a.pos == b.pos then
    return a.len < b.len
  end
  return a.pos < b.pos
end

local function capability_supported(client, capability)
  local value = client and client.capabilities and client.capabilities[capability]
  if value == nil then
    return false
  end
  if type(value) == "boolean" then
    return value
  end
  return true
end

local function capability_config(client, capability)
  local value = client and client.capabilities and client.capabilities[capability]
  return type(value) == "table" and value or nil
end

local function navigation_config_hint()
  local hints = {}
  if USERDIR then
    hints[#hints + 1] = common.home_encode(join_path(USERDIR, "lsp.json"))
  end
  local project = core.root_project()
  if project then
    hints[#hints + 1] = common.home_encode(join_path(project.path, "lsp.json"))
  end
  return #hints > 0 and table.concat(hints, " or ") or "lsp.json"
end

local function capture_view_location(view)
  if not (view and view.doc and view.doc.abs_filename) then
    return nil
  end
  local line1, col1, line2, col2 = view.doc:get_selection()
  return {
    path = view.doc.abs_filename,
    line1 = line1,
    col1 = col1,
    line2 = line2,
    col2 = col2,
  }
end

local function push_location_history(location)
  if not location then
    return
  end
  local prev = manager.location_history[#manager.location_history]
  if prev
      and prev.path == location.path
      and prev.line1 == location.line1
      and prev.col1 == location.col1
      and prev.line2 == location.line2
      and prev.col2 == location.col2 then
    return
  end
  manager.location_history[#manager.location_history + 1] = location
  if #manager.location_history > 200 then
    table.remove(manager.location_history, 1)
  end
end

local function open_captured_location(location)
  if not (location and location.path) then
    return false
  end
  local doc = core.open_doc(location.path)
  local docview = core.root_view:open_doc(doc)
  doc:set_selection(location.line1, location.col1, location.line2, location.col2)
  docview:scroll_to_line(location.line1, true, true)
  return true
end

local function location_to_target(location)
  if location.targetUri then
    return location.targetUri, location.targetSelectionRange or location.targetRange or location.range
  end
  return location.uri, location.range
end

local function open_location(uri, range, options)
  options = options or {}
  local abs_path = uri_to_path(uri)
  if not abs_path or not range then
    core.warn("LSP returned an unsupported location")
    return
  end
  if options.history then
    push_location_history(options.history)
  end
  local doc = core.open_doc(abs_path)
  local line, col = doc_position_from_lsp(doc, range.start)
  local end_line, end_col = doc_position_from_lsp(doc, range["end"])
  local docview = core.root_view:open_doc(doc)
  doc:set_selection(line, col, end_line, end_col)
  docview:scroll_to_line(line, true, true)
end

local function navigation_client(doc, capability, action)
  local spec = manager.find_spec_for_doc(doc)
  if not spec then
    local label = doc.syntax and doc.syntax.name or doc:get_name()
    core.warn("No LSP server configured for %s. Add a server in %s.", label, navigation_config_hint())
    return nil
  end
  local client = manager.open_doc(doc)
  if not client then
    return nil
  end
  if client.is_initialized and capability and not capability_supported(client, capability) then
    core.warn("LSP server %s does not support %s", client.name, action)
    return nil
  end
  return client
end

local function content_to_text(content)
  if type(content) == "string" then
    return content
  end
  if type(content) ~= "table" then
    return ""
  end
  if content.kind and content.value then
    return content.value
  end
  local parts = {}
  for _, item in ipairs(content) do
    parts[#parts + 1] = content_to_text(item)
  end
  return table.concat(parts, "\n")
end

local function fuzzy_items(items, text)
  return native_picker.rank_items(items, text or "", "text")
end

local function pick_from_list(label, items, on_submit)
  if #items == 0 then
    core.warn("%s: no results", label)
    return
  end
  core.command_view:enter(label, {
    submit = function(text, item)
      local selected = item or fuzzy_items(items, text)[1]
      if selected then
        on_submit(selected.payload or selected)
      end
    end,
    suggest = function(text)
      return fuzzy_items(items, text)
    end,
  })
end

local function range_sort_desc(a, b)
  local ar = a.range.start
  local br = b.range.start
  if ar.line == br.line then
    return ar.character > br.character
  end
  return ar.line > br.line
end

local function set_cursor_after_insert(doc, line, col, text)
  local end_line, end_col
  if text and text ~= "" then
    end_line, end_col = doc:position_offset(line, col, #text)
  else
    end_line, end_col = line, col
  end
  doc:set_selection(end_line, end_col, end_line, end_col)
end

local function apply_text_edit(doc, edit, move_cursor)
  local start_line, start_col = doc_position_from_lsp(doc, edit.range.start)
  local end_line, end_col = doc_position_from_lsp(doc, edit.range["end"])
  doc:remove(start_line, start_col, end_line, end_col)
  if edit.newText and edit.newText ~= "" then
    doc:insert(start_line, start_col, edit.newText)
  end
  if move_cursor then
    set_cursor_after_insert(doc, start_line, start_col, edit.newText or "")
  end
end

local function apply_workspace_edit(edit)
  if type(edit) ~= "table" then
    return
  end

  local grouped = {}

  if type(edit.changes) == "table" then
    for uri, edits in pairs(edit.changes) do
      grouped[uri_to_path(uri)] = edits
    end
  end

  if type(edit.documentChanges) == "table" then
    for _, change in ipairs(edit.documentChanges) do
      local doc_uri = change.textDocument and change.textDocument.uri
      if doc_uri and change.edits then
        grouped[uri_to_path(doc_uri)] = change.edits
      end
    end
  end

  for abs_path, edits in pairs(grouped) do
    if abs_path then
      local doc = core.open_doc(abs_path)
      table.sort(edits, range_sort_desc)
      local native_edits = {}
      for _, item in ipairs(edits) do
        local start_line, start_col = doc_position_from_lsp(doc, item.range.start)
        local end_line, end_col = doc_position_from_lsp(doc, item.range["end"])
        native_edits[#native_edits + 1] = {
          line1 = start_line,
          col1 = start_col,
          line2 = end_line,
          col2 = end_col,
          text = item.newText or "",
        }
      end
      if not doc:apply_edits(native_edits) then
        for _, item in ipairs(edits) do
          apply_text_edit(doc, item)
        end
      end
      core.root_view:open_doc(doc)
    end
  end
end

local function execute_lsp_command(client, command_info)
  if not command_info or not command_info.command then
    return
  end
  client:request("workspace/executeCommand", {
    command = command_info.command,
    arguments = command_info.arguments,
  }, function(_, err)
    if err then
      core.warn("LSP executeCommand failed: %s", err.message or tostring(err))
    end
  end)
end

local function format_action_kind(kind)
  if type(kind) ~= "string" or kind == "" then
    return ""
  end
  local tail = kind:match("([^%.]+)$") or kind
  tail = tail:gsub("^%l", string.upper)
  tail = tail:gsub("(%u)", " %1"):gsub("^%s+", "")
  return tail
end

local function action_score(action)
  local score = 0
  if action.isPreferred then
    score = score + 1000
  end
  if action.kind == "quickfix" then
    score = score + 100
  elseif type(action.kind) == "string" and action.kind:match("^quickfix%.") then
    score = score + 80
  elseif type(action.kind) == "string" and action.kind:match("^source%.fixAll") then
    score = score + 60
  end
  if action.disabled then
    score = score - 500
  end
  return score
end

local function sort_actions(actions)
  table.sort(actions, function(a, b)
    local as = action_score(a)
    local bs = action_score(b)
    if as == bs then
      return tostring(a.title or "") < tostring(b.title or "")
    end
    return as > bs
  end)
  return actions
end

local function resolve_code_action(client, action, callback)
  local provider = capability_config(client, "codeActionProvider")
  if not (provider and provider.resolveProvider and action and action.data) then
    callback(action)
    return
  end
  client:request("codeAction/resolve", action, function(result, err)
    if err then
      core.warn("LSP code action resolve failed: %s", err.message or tostring(err))
      callback(action)
      return
    end
    callback(result or action)
  end)
end

local function apply_code_action(client, action)
  if not action or action.disabled then
    local disabled = action and action.disabled
    local reason = type(disabled) == "table" and disabled.reason or disabled
    if reason and tostring(reason) ~= "" then
      core.warn("LSP action unavailable: %s", tostring(reason))
    else
      core.warn("LSP action unavailable")
    end
    return
  end

  resolve_code_action(client, action, function(resolved)
    if resolved.edit then
      apply_workspace_edit(resolved.edit)
    end
    if resolved.command then
      execute_lsp_command(client, resolved.command)
    elseif resolved.command == nil and resolved.arguments and resolved.title == nil then
      execute_lsp_command(client, resolved)
    end
  end)
end

local function code_action_items(actions)
  local items = {}
  for i = 1, #(actions or {}) do
    local action = actions[i]
    local kind = format_action_kind(action.kind)
    local info = kind
    if action.isPreferred then
      info = (info ~= "" and (info .. " · preferred") or "preferred")
    end
    if action.disabled then
      local reason = type(action.disabled) == "table" and action.disabled.reason or "unavailable"
      info = (info ~= "" and (info .. " · ") or "") .. tostring(reason)
    end
    items[#items + 1] = {
      text = string.format("%03d %s", i, action.title or "Code Action"),
      info = info,
      payload = action,
    }
  end
  return items
end

local function apply_text_edits_to_doc(doc, edits)
  if type(edits) ~= "table" or #edits == 0 then
    return
  end
  table.sort(edits, range_sort_desc)
  for i = 1, #edits do
    apply_text_edit(doc, edits[i])
  end
end

local function publish_diagnostics(client, params)
  local uri = params.uri
  local abs_path = uri_to_path(uri)
  local tracked_state
  if abs_path then
    for doc, state in pairs(manager.doc_state) do
      if state.uri == uri then
        tracked_state = state
        break
      end
    end
  end

  local incoming_version = params.version
  if tracked_state and incoming_version ~= nil and tracked_state.version ~= nil
      and incoming_version < tracked_state.version then
    return
  end

  native_lsp.publish_diagnostics(uri, incoming_version, params.diagnostics or {})
  if tracked_state then
    tracked_state.last_diagnostic_version = incoming_version or tracked_state.version
  end
  local path = uri_to_path(uri)
  local label = path and basename(path) or uri
  core.status_view:show_message("!", style.accent, string.format(
    "LSP %s: %d diagnostic(s) for %s",
    client.name,
    #(params.diagnostics or {}),
    label
  ))
  core.redraw = true
end

local function diagnostic_sorter(a, b)
  local ar = a.range and a.range.start or {}
  local br = b.range and b.range.start or {}
  if (ar.line or 0) == (br.line or 0) then
    return (ar.character or 0) < (br.character or 0)
  end
  return (ar.line or 0) < (br.line or 0)
end

local function diagnostic_line_range(diagnostic)
  local range = diagnostic and diagnostic.range
  if not range then
    return nil, nil
  end
  return (range.start.line or 0) + 1, (range["end"].line or range.start.line or 0) + 1
end

local function diagnostic_intersects_range(doc, diagnostic, line1, col1, line2, col2)
  local range = diagnostic and diagnostic.range
  if not range then
    return false
  end
  local start_line, start_col = doc_position_from_lsp(doc, range.start)
  local end_line, end_col = doc_position_from_lsp(doc, range["end"])
  if end_line < start_line or (end_line == start_line and end_col < start_col) then
    end_line, end_col = start_line, start_col
  end
  if line2 < line1 or (line2 == line1 and col2 < col1) then
    line2, col2 = line1, col1
  end
  local starts_before_selection_end = start_line < line2 or (start_line == line2 and start_col <= col2)
  local ends_after_selection_start = end_line > line1 or (end_line == line1 and end_col >= col1)
  return starts_before_selection_end and ends_after_selection_start
end

local function get_doc_diagnostics(doc)
  if not doc or not doc.abs_filename then
    return {}
  end
  local uri = path_to_uri(doc.abs_filename)
  return native_lsp.get_diagnostics(uri) or {}
end

local function get_sorted_doc_diagnostics(doc)
  if not doc or not doc.abs_filename then
    return {}
  end
  local uri = path_to_uri(doc.abs_filename)
  return native_lsp.get_sorted_diagnostics(uri) or {}
end

local function diagnostics_for_range(doc, uri, line1, col1, line2, col2)
  local diagnostics = {}
  for _, diagnostic in ipairs(get_doc_diagnostics(doc)) do
    if diagnostic_intersects_range(doc, diagnostic, line1, col1, line2, col2) then
      diagnostics[#diagnostics + 1] = diagnostic
    end
  end
  if #diagnostics == 0 then
    return get_doc_diagnostics(doc)
  end
  table.sort(diagnostics, function(a, b)
    local sa = a.severity or 3
    local sb = b.severity or 3
    if sa == sb then
      return diagnostic_sorter(a, b)
    end
    return sa < sb
  end)
  return diagnostics
end

local function handle_server_notification(client, message)
  if message.method == "textDocument/publishDiagnostics" then
    publish_diagnostics(client, message.params or {})
  end
end

function manager.reload_config()
  local project = core.root_project()
  manager.raw_config = {}
  manager.specs = {}
  manager.clients = {}
  manager.doc_state = setmetatable({}, { __mode = "k" })
  manager.diagnostics = {}
  manager.config_paths = {}
  if USERDIR then
    manager.config_paths[#manager.config_paths + 1] = join_path(USERDIR, "lsp.json")
  end
  if project then
    manager.config_paths[#manager.config_paths + 1] = join_path(project.path, "lsp.json")
  end
  manager.config_path = manager.config_paths[#manager.config_paths]

  local ok, count = pcall(native_lsp.reload_config, manager.config_paths)
  if not ok then
    core.warn("Failed to parse lsp.json: %s", count)
    return false
  end
  return count > 0
end

function manager.start_semantic_refresh_loop()
  if manager.semantic_refresh_thread_started then
    return
  end
  manager.semantic_refresh_thread_started = true
  core.add_thread(function()
    while true do
      local now = system.get_time()
      local due = native_lsp.take_due_semantic(now)
      for _, uri in ipairs(due) do
        for doc, state in pairs(manager.doc_state) do
          if state.uri == uri then
            manager.request_semantic_tokens(doc)
          end
        end
      end
      coroutine.yield(0.1)
    end
  end)
end

function manager.find_spec_for_doc(doc)
  if not doc or not doc.abs_filename or not doc.syntax or not doc.syntax.name then
    return nil
  end
  local project = core.root_project()
  if not project then
    return nil
  end
  local spec = native_lsp.find_spec(doc.syntax.name:lower(), doc.abs_filename, project.path)
  if spec then
    return spec, spec.root_dir
  end
  return nil
end

function manager.ensure_client(doc)
  local spec, root_dir = manager.find_spec_for_doc(doc)
  if not spec or not root_dir then
    return nil
  end
  local key = spec.name .. "@" .. root_dir
  if manager.clients[key] then
    return manager.clients[key]
  end

  local client, err = Client.new(spec.name, spec, root_dir, {
    on_notification = handle_server_notification,
    on_exit = function(exited_client)
      manager.clients[key] = nil
      for tracked_doc, state in pairs(manager.doc_state) do
        if state.client == exited_client then
          manager.doc_state[tracked_doc] = nil
        end
      end
    end,
  })
  if not client then
    core.warn("Failed to start LSP %s: %s", spec.name, err)
    return nil
  end

  client:initialize({
    processId = nil,
    clientInfo = {
      name = "lite-anvil",
      version = VERSION,
    },
    rootPath = root_dir,
    rootUri = path_to_uri(root_dir),
    workspaceFolders = {
      { uri = path_to_uri(root_dir), name = basename(root_dir) },
    },
    initializationOptions = spec.initializationOptions,
    capabilities = {
      general = {
        positionEncodings = { "utf-8", "utf-16" },
      },
      textDocument = {
        synchronization = {
          didSave = true,
          willSave = false,
          willSaveWaitUntil = false,
        },
        hover = {
          contentFormat = { "plaintext", "markdown" },
        },
        definition = {
          dynamicRegistration = false,
        },
        rename = {
          dynamicRegistration = false,
          prepareSupport = false,
        },
        completion = {
          dynamicRegistration = false,
          completionItem = {
            snippetSupport = false,
            documentationFormat = { "plaintext", "markdown" },
          },
        },
        semanticTokens = {
          dynamicRegistration = false,
          requests = {
            full = { delta = false },
            range = false,
          },
          tokenTypes = {
            "namespace", "type", "class", "enum", "interface", "struct", "typeParameter",
            "parameter", "variable", "property", "enumMember", "event", "function", "method",
            "macro", "keyword", "modifier", "comment", "string", "number", "regexp",
            "operator", "decorator"
          },
          tokenModifiers = {},
          formats = { "relative" },
          overlappingTokenSupport = false,
          multilineTokenSupport = true,
        },
      },
      workspace = {
        workspaceEdit = {
          documentChanges = true,
        },
      },
    },
  }, function(ready_client)
    if spec.settings ~= nil then
      ready_client:notify("workspace/didChangeConfiguration", {
        settings = spec.settings,
      })
    end
  end)

  manager.clients[key] = client
  return client
end

local function apply_semantic_tokens(doc, client, semantic_result)
  local data = semantic_result and semantic_result.data
  if type(data) ~= "table" then
    return
  end

  local state = manager.doc_state[doc]
  if not state then
    return
  end

  local provider = client.capabilities and client.capabilities.semanticTokensProvider
  local token_types = provider and provider.legend and provider.legend.tokenTypes or {}
  local lines = {}

  lines = native_lsp.publish_semantic(token_types, data) or {}

  for line_no in pairs(state.semantic_lines or {}) do
    if not lines[line_no] then
      doc.highlighter:merge_line(line_no, nil)
      state.semantic_lines[line_no] = nil
      core.redraw = true
    end
  end

  for line_no, positioned in pairs(lines) do
    table.sort(positioned, compare_positioned_token)
    local before = doc.highlighter:get_line_signature(line_no)
    doc.highlighter:merge_line(line_no, positioned)
    state.semantic_lines[line_no] = true
    if before ~= doc.highlighter:get_line_signature(line_no) then
      core.redraw = true
    end
  end
end

function manager.request_semantic_tokens(doc)
  local state = manager.doc_state[doc]
  if not state or state.semantic_request_in_flight then
    return
  end
  if config.plugins.lsp.semantic_highlighting == false then
    return
  end

  local client = state.client
  local provider = client and client.capabilities and client.capabilities.semanticTokensProvider
  if not provider or not provider.full then
    return
  end

  state.semantic_request_in_flight = true
  client:request("textDocument/semanticTokens/full", {
    textDocument = { uri = state.uri },
  }, function(result, err)
    state.semantic_request_in_flight = false
    if err then
      core.warn("LSP semantic tokens failed: %s", err.message or tostring(err))
      return
    end
    if result then
      apply_semantic_tokens(doc, client, result)
    end
  end)
end

function manager.schedule_semantic_refresh(doc, delay)
  local state = manager.doc_state[doc]
  if not state then
    return
  end
  native_lsp.schedule_semantic(state.uri, system.get_time(), delay or 0.35)
end

function manager.open_doc(doc)
  if doc.large_file_mode then
    return nil
  end
  local client = manager.ensure_client(doc)
  if not client or manager.doc_state[doc] then
    return client
  end

  manager.doc_state[doc] = {
    client = client,
    uri = path_to_uri(doc.abs_filename),
    version = doc:get_change_id(),
    semantic_lines = {},
    last_diagnostic_version = nil,
  }
  native_lsp.open_doc(manager.doc_state[doc].uri, manager.doc_state[doc].version)

  client:notify("textDocument/didOpen", {
    textDocument = {
      uri = manager.doc_state[doc].uri,
      languageId = doc.syntax and doc.syntax.name and doc.syntax.name:lower() or "plaintext",
      version = manager.doc_state[doc].version,
      text = full_document_text(doc),
    },
  })

  return client
end

function manager.on_doc_change(doc)
  local state = manager.doc_state[doc]
  if not state then
    state = manager.open_doc(doc) and manager.doc_state[doc]
  end
  if not state then
    return
  end

  state.version = doc:get_change_id()
  native_lsp.update_doc(state.uri, state.version)
  state.client:notify("textDocument/didChange", {
    textDocument = {
      uri = state.uri,
      version = state.version,
    },
    contentChanges = {
      { text = full_document_text(doc) },
    },
  })
  manager.schedule_semantic_refresh(doc, 0.35)
  core.redraw = true
end

function manager.on_doc_save(doc)
  local state = manager.doc_state[doc]
  if not state then
    return
  end
  state.client:notify("textDocument/didSave", {
    textDocument = { uri = state.uri },
    text = full_document_text(doc),
  })
  manager.schedule_semantic_refresh(doc, 0.1)
  core.redraw = true
end

function manager.on_doc_close(doc)
  local state = manager.doc_state[doc]
  if not state then
    return
  end
  state.client:notify("textDocument/didClose", {
    textDocument = { uri = state.uri },
  })
  native_lsp.close_doc(state.uri)
  manager.doc_state[doc] = nil
  core.redraw = true
end

function manager.document_params(doc, line, col)
  line = line or select(1, doc:get_selection())
  col = col or select(2, doc:get_selection())
  return {
    textDocument = { uri = path_to_uri(doc.abs_filename) },
    position = lsp_position_from_doc(doc, line, col),
  }
end

function manager.goto_definition()
  local view = current_docview()
  if not view or not view.doc.abs_filename then
    return
  end
  local client = navigation_client(view.doc, "definitionProvider", "goto definition")
  if not client then
    return
  end
  local origin = capture_view_location(view)

  client:request("textDocument/definition", manager.document_params(view.doc), function(result, err)
    if err then
      core.warn("LSP definition failed: %s", err.message or tostring(err))
      return
    end
    if not result then
      core.warn("LSP definition returned no result")
      return
    end

    local items = make_location_items(result[1] and result or { result })
    if #items == 1 then
      open_location(items[1].payload.uri, items[1].payload.range, { history = origin })
    else
      pick_from_list("Definitions", items, function(item)
        open_location(item.uri, item.range, { history = origin })
      end)
    end
  end)
end

local function goto_location_request(method, empty_message, capability, action)
  local view = current_docview()
  if not view or not view.doc.abs_filename then
    return
  end
  local client = navigation_client(view.doc, capability, action)
  if not client then
    return
  end
  local origin = capture_view_location(view)
  client:request(method, manager.document_params(view.doc), function(result, err)
    if err then
      core.warn("LSP request %s failed: %s", method, err.message or tostring(err))
      return
    end
    if not result then
      core.warn(empty_message)
      return
    end
    local items = make_location_items(result[1] and result or { result })
    if #items == 1 then
      open_location(items[1].payload.uri, items[1].payload.range, { history = origin })
    else
      pick_from_list(method, items, function(item)
        open_location(item.uri, item.range, { history = origin })
      end)
    end
  end)
end

function manager.goto_type_definition()
  goto_location_request(
    "textDocument/typeDefinition",
    "LSP type definition returned no result",
    "typeDefinitionProvider",
    "goto type definition"
  )
end

function manager.goto_implementation()
  goto_location_request(
    "textDocument/implementation",
    "LSP implementation returned no result",
    "implementationProvider",
    "goto implementation"
  )
end

function manager.jump_back()
  local current = capture_view_location(current_docview())
  while #manager.location_history > 0 do
    local location = table.remove(manager.location_history)
    if not current
        or current.path ~= location.path
        or current.line1 ~= location.line1
        or current.col1 ~= location.col1
        or current.line2 ~= location.line2
        or current.col2 ~= location.col2 then
      return open_captured_location(location)
    end
  end
  core.warn("LSP jump history is empty")
  return false
end

function manager.hover()
  local view = current_docview()
  if not view or not view.doc.abs_filename then
    return
  end
  local client = manager.open_doc(view.doc)
  if not client then
    core.warn("No LSP server configured for %s", view.doc:get_name())
    return
  end

  client:request("textDocument/hover", manager.document_params(view.doc), function(result, err)
    if err then
      core.warn("LSP hover failed: %s", err.message or tostring(err))
      return
    end
    local text = result and content_to_text(result.contents)
    if not text or text == "" then
      core.warn("LSP hover returned no information")
      return
    end
    core.status_view:show_message("i", style.text, text:gsub("%s+", " "):sub(1, 240))
  end)
end

function manager.show_diagnostics()
  local view = current_docview()
  if not view or not view.doc.abs_filename then
    return
  end
  local uri = path_to_uri(view.doc.abs_filename)
  local diagnostics = get_sorted_doc_diagnostics(view.doc)
  if #diagnostics == 0 then
    core.log("No LSP diagnostics for %s", view.doc:get_name())
    return
  end
  local items = {}
  for i = 1, #diagnostics do
    local diagnostic = diagnostics[i]
    local range = diagnostic.range
    if range then
      items[#items + 1] = {
        text = string.format("%03d L%d:%d %s", i, (range.start.line or 0) + 1,
          (range.start.character or 0) + 1, (diagnostic.message or ""):gsub("%s+", " "):sub(1, 100)),
        info = tostring(diagnostic.source or diagnostic.code or ""),
        payload = { uri = uri, range = range },
      }
    end
  end
  pick_from_list("Diagnostics", items, function(item)
    open_location(item.uri, item.range)
  end)
end

function manager.get_line_diagnostic_severity(doc, line)
  if doc and doc.abs_filename then
    return native_lsp.get_line_diagnostic_severity(path_to_uri(doc.abs_filename), line)
  end
  local diagnostics = get_doc_diagnostics(doc)
  local severity
  for i = 1, #diagnostics do
    local start_line, end_line = diagnostic_line_range(diagnostics[i])
    if start_line and line >= start_line and line <= end_line then
      local current = diagnostics[i].severity or 3
      if not severity or current < severity then
        severity = current
      end
    end
  end
  return severity
end

function manager.get_line_diagnostic_segments(doc, line)
  local diagnostics = get_doc_diagnostics(doc)
  if #diagnostics == 0 then
    return nil
  end

  local segments = {}
  for i = 1, #diagnostics do
    local diagnostic = diagnostics[i]
    local range = diagnostic.range
    local start_line, end_line = diagnostic_line_range(diagnostic)
    if range and start_line and line >= start_line and line <= end_line then
      local line_text = doc.lines[line] or "\n"
      local max_col = math.max(1, #line_text)
      local col1 = 1
      local col2 = max_col
      if line == start_line then
        col1 = select(2, doc_position_from_lsp(doc, range.start))
      end
      if line == end_line then
        col2 = select(2, doc_position_from_lsp(doc, range["end"]))
      end
      col1 = common.clamp(col1, 1, max_col)
      col2 = common.clamp(math.max(col1 + 1, col2), 1, max_col)
      segments[#segments + 1] = {
        col1 = col1,
        col2 = col2,
        severity = diagnostic.severity or 3,
      }
    end
  end

  table.sort(segments, function(a, b)
    if a.col1 == b.col1 then
      return a.col2 < b.col2
    end
    return a.col1 < b.col1
  end)

  return #segments > 0 and segments or nil
end

function manager.get_hover_diagnostic(doc, line, col)
  local diagnostics = get_doc_diagnostics(doc)
  if #diagnostics == 0 then
    return nil
  end

  local best = nil
  for i = 1, #diagnostics do
    local diagnostic = diagnostics[i]
    local range = diagnostic.range
    local start_line, end_line = diagnostic_line_range(diagnostic)
    if range and start_line and line >= start_line and line <= end_line then
      local start_col = 1
      local end_col = math.huge
      if line == start_line then
        start_col = select(2, doc_position_from_lsp(doc, range.start))
      end
      if line == end_line then
        end_col = select(2, doc_position_from_lsp(doc, range["end"]))
      end

      local on_line = col == nil
      local within = on_line or (col >= start_col and col <= math.max(start_col + 1, end_col))
      if within then
        local severity = diagnostic.severity or 3
        if not best
            or severity < (best.severity or 3)
            or ((severity == (best.severity or 3)) and #tostring(diagnostic.message or "") > #tostring(best.message or "")) then
          best = diagnostic
        end
      end
    end
  end

  return best
end

function manager.get_inline_diagnostic(doc, line)
  local diagnostics = get_doc_diagnostics(doc)
  if #diagnostics == 0 then
    return nil
  end

  local best = nil
  local best_end_col = nil
  for i = 1, #diagnostics do
    local diagnostic = diagnostics[i]
    local range = diagnostic.range
    local start_line, end_line = diagnostic_line_range(diagnostic)
    if range and start_line and start_line == line then
      local severity = diagnostic.severity or 3
      local end_col = 1
      if line == end_line then
        end_col = select(2, doc_position_from_lsp(doc, range["end"]))
      else
        end_col = select(2, doc_position_from_lsp(doc, range.start))
      end
      if not best
          or severity < (best.severity or 3)
          or ((severity == (best.severity or 3)) and #tostring(diagnostic.message or "") > #tostring(best.message or "")) then
        best = diagnostic
        best_end_col = end_col
      end
    end
  end

  if not best then
    return nil
  end

  return best, best_end_col
end

make_location_items = function(locations)
  local items = {}
  for i = 1, #locations do
    local location = locations[i]
    local uri, range = location_to_target(location)
    local path = uri_to_path(uri)
    if path and range then
      local line = (range.start.line or 0) + 1
      local col = (range.start.character or 0) + 1
      items[#items + 1] = {
        text = string.format("%03d %s:%d:%d", i, basename(path), line, col),
        info = common.home_encode(path),
        payload = { uri = uri, range = range },
      }
    end
  end
  return items
end

local function flatten_document_symbols(symbols, uri, out, prefix)
  out = out or {}
  prefix = prefix or ""
  for i = 1, #(symbols or {}) do
    local symbol = symbols[i]
    local name = prefix ~= "" and (prefix .. " / " .. (symbol.name or "?")) or (symbol.name or "?")
    local range = symbol.selectionRange or symbol.range or (symbol.location and symbol.location.range)
    local symbol_uri = symbol.uri or (symbol.location and symbol.location.uri) or uri
    if range and symbol_uri then
      out[#out + 1] = {
        text = string.format("%s", name),
        info = symbol.detail or tostring(symbol.kind or ""),
        payload = { uri = symbol_uri, range = range },
      }
    end
    if symbol.children then
      flatten_document_symbols(symbol.children, symbol_uri, out, name)
    end
  end
  return out
end

function manager.find_references()
  local view = current_docview()
  if not view or not view.doc.abs_filename then
    return
  end
  local client = navigation_client(view.doc, "referencesProvider", "find references")
  if not client then
    return
  end
  local origin = capture_view_location(view)
  local params = manager.document_params(view.doc)
  params.context = { includeDeclaration = true }
  client:request("textDocument/references", params, function(result, err)
    if err then
      core.warn("LSP references failed: %s", err.message or tostring(err))
      return
    end
    local items = make_location_items(result or {})
    pick_from_list("References", items, function(item)
      open_location(item.uri, item.range, { history = origin })
    end)
  end)
end

function manager.show_document_symbols()
  local view = current_docview()
  if not view or not view.doc.abs_filename then
    return
  end
  local client = navigation_client(view.doc, "documentSymbolProvider", "show document symbols")
  if not client then
    return
  end
  local origin = capture_view_location(view)
  client:request("textDocument/documentSymbol", {
    textDocument = { uri = path_to_uri(view.doc.abs_filename) },
  }, function(result, err)
    if err then
      core.warn("LSP document symbols failed: %s", err.message or tostring(err))
      return
    end
    local items = flatten_document_symbols(result or {}, path_to_uri(view.doc.abs_filename))
    pick_from_list("Document Symbols", items, function(item)
      open_location(item.uri, item.range, { history = origin })
    end)
  end)
end

function manager.request_code_actions(options)
  options = options or {}
  local view = current_docview()
  if not view or not view.doc.abs_filename then
    return
  end
  local client = navigation_client(view.doc, "codeActionProvider", options.only and "quick fixes" or "code actions")
  if not client then
    return
  end
  local line1, col1, line2, col2
  if options.line then
    line1 = options.line
    col1 = options.col1 or 1
    line2 = options.line2 or options.line
    col2 = options.col2 or math.max(#(view.doc.lines[line2] or "\n"), 1)
  else
    line1, col1, line2, col2 = view.doc:get_selection(true)
  end
  local uri = path_to_uri(view.doc.abs_filename)
  local context = {
    diagnostics = diagnostics_for_range(view.doc, uri, line1, col1, line2, col2),
  }
  if options.only then
    context.only = options.only
  end
  client:request("textDocument/codeAction", {
    textDocument = { uri = uri },
    range = {
      start = lsp_position_from_doc(view.doc, line1, col1),
      ["end"] = lsp_position_from_doc(view.doc, line2, col2),
    },
    context = context,
  }, function(result, err)
    if err then
      core.warn("LSP code action failed: %s", err.message or tostring(err))
      return
    end
    local actions = {}
    for i = 1, #(result or {}) do
      local action = result[i]
      if not action.disabled then
        actions[#actions + 1] = action
      end
    end
    sort_actions(actions)
    if #actions == 0 then
      core.warn("%s: no results", options.label or "Code Actions")
      return
    end
    if options.auto_apply_single and #actions == 1 then
      apply_code_action(client, actions[1])
      return
    end
    pick_from_list(options.label or "Code Actions", code_action_items(actions), function(action)
      apply_code_action(client, action)
    end)
  end)
end

function manager.code_action()
  return manager.request_code_actions({})
end

function manager.quick_fix()
  return manager.request_code_actions({
    only = { "quickfix" },
    auto_apply_single = true,
    label = "Quick Fixes",
  })
end

function manager.quick_fix_for_line(line)
  return manager.request_code_actions({
    only = { "quickfix" },
    auto_apply_single = true,
    label = "Quick Fixes",
    line = line,
  })
end

function manager.signature_help()
  local view = current_docview()
  if not view or not view.doc.abs_filename then
    return
  end
  local client = manager.open_doc(view.doc)
  if not client then
    core.warn("No LSP server configured for %s", view.doc:get_name())
    return
  end
  client:request("textDocument/signatureHelp", manager.document_params(view.doc), function(result, err)
    if err then
      core.warn("LSP signature help failed: %s", err.message or tostring(err))
      return
    end
    if not result or not result.signatures or #result.signatures == 0 then
      core.warn("LSP signature help returned no signatures")
      return
    end
    local active_signature = (result.activeSignature or 0) + 1
    local signature = result.signatures[active_signature] or result.signatures[1]
    local label = signature.label or ""
    local active_parameter = (result.activeParameter or 0) + 1
    if signature.parameters and signature.parameters[active_parameter] and signature.parameters[active_parameter].label then
      local parameter = signature.parameters[active_parameter].label
      if type(parameter) == "string" then
        label = label:gsub(parameter, "[" .. parameter .. "]", 1)
      end
    end
    core.status_view:show_message("i", style.text, label:sub(1, 240))
  end)
end

function manager.maybe_trigger_signature_help(text)
  if text ~= "(" and text ~= "," then
    return
  end
  local view = current_docview()
  if not view or not view.doc.abs_filename then
    return
  end
  local state = manager.doc_state[view.doc]
  if not state then
    return
  end
  local client = state.client
  local provider = client and client.capabilities and client.capabilities.signatureHelpProvider
  if not provider then
    return
  end
  local triggers = provider.triggerCharacters or {}
  local matched = #triggers == 0
  for i = 1, #triggers do
    if triggers[i] == text then
      matched = true
      break
    end
  end
  if matched then
    manager.signature_help()
  end
end

function manager.maybe_trigger_completion(text)
  local view = current_docview()
  if not view or not view.doc.abs_filename then
    return
  end
  local state = manager.doc_state[view.doc]
  if not state then
    return
  end
  local client = state.client
  local provider = client and client.capabilities and client.capabilities.completionProvider
  if not provider then
    return
  end

  local triggers = provider.triggerCharacters or {}
  if #triggers == 0 then
    return
  end

  local trigger_text = text
  if text == ":" then
    local line, col = view.doc:get_selection()
    if col > 1 and view.doc:get_char(line, col - 1) == ":" then
      trigger_text = "::"
    end
  end

  for i = 1, #triggers do
    if triggers[i] == trigger_text or triggers[i] == text then
      manager.complete()
      break
    end
  end
end

function manager.format_document_for(doc, callback)
  callback = callback or function() end
  local target_doc = doc or (current_docview() and current_docview().doc)
  if not target_doc or not target_doc.abs_filename then
    callback(false)
    return
  end
  local client = manager.open_doc(target_doc)
  if not client then
    callback(false)
    return
  end
  client:request("textDocument/formatting", {
    textDocument = { uri = path_to_uri(target_doc.abs_filename) },
    options = {
      tabSize = select(2, target_doc:get_indent_info()),
      insertSpaces = select(1, target_doc:get_indent_info()) ~= "hard",
    },
  }, function(result, err)
    if err then
      core.warn("LSP format document failed: %s", err.message or tostring(err))
      callback(false, err)
      return
    end
    apply_text_edits_to_doc(target_doc, result or {})
    callback(true)
  end)
end

function manager.format_document()
  local view = current_docview()
  if not view or not view.doc.abs_filename then
    return
  end
  manager.format_document_for(view.doc)
end

function manager.format_selection()
  local view = current_docview()
  if not view or not view.doc.abs_filename then
    return
  end
  local client = manager.open_doc(view.doc)
  if not client then
    core.warn("No LSP server configured for %s", view.doc:get_name())
    return
  end
  local line1, col1, line2, col2 = view.doc:get_selection(true)
  client:request("textDocument/rangeFormatting", {
    textDocument = { uri = path_to_uri(view.doc.abs_filename) },
    range = {
      start = lsp_position_from_doc(view.doc, line1, col1),
      ["end"] = lsp_position_from_doc(view.doc, line2, col2),
    },
    options = {
      tabSize = select(2, view.doc:get_indent_info()),
      insertSpaces = select(1, view.doc:get_indent_info()) ~= "hard",
    },
  }, function(result, err)
    if err then
      core.warn("LSP format selection failed: %s", err.message or tostring(err))
      return
    end
    apply_text_edits_to_doc(view.doc, result or {})
  end)
end

function manager.workspace_symbols()
  local view = current_docview()
  local doc = view and view.doc
  local client = doc and manager.open_doc(doc)
  if not client then
    core.warn("No LSP server configured for workspace symbol search")
    return
  end
  core.command_view:enter("Workspace Symbols", {
    submit = function(text)
      if text == "" then
        return
      end
      client:request("workspace/symbol", { query = text }, function(result, err)
        if err then
          core.warn("LSP workspace symbols failed: %s", err.message or tostring(err))
          return
        end
        local items = {}
        for i = 1, #(result or {}) do
          local symbol = result[i]
          local uri, range = location_to_target(symbol.location or symbol)
          if uri and range then
            items[#items + 1] = {
              text = string.format("%03d %s", i, symbol.name or "?"),
              info = symbol.containerName or "",
              payload = { uri = uri, range = range },
            }
          end
        end
        pick_from_list("Workspace Symbols", items, function(item)
          open_location(item.uri, item.range)
        end)
      end)
    end,
    suggest = function()
      return {}
    end,
    show_suggestions = false,
  })
end

local function goto_diagnostic(forward)
  local view = current_docview()
  if not view or not view.doc.abs_filename then
    return
  end

  local diagnostics = {}
  for i = 1, #get_sorted_doc_diagnostics(view.doc) do
    local diagnostic = get_sorted_doc_diagnostics(view.doc)[i]
    if diagnostic.range and diagnostic.range.start then
      diagnostics[#diagnostics + 1] = diagnostic
    end
  end
  if #diagnostics == 0 then
    core.warn("No LSP diagnostics for %s", view.doc:get_name())
    return
  end

  local line, col = view.doc:get_selection()
  local current_line = line - 1
  local current_char = byte_to_utf8_char(view.doc.lines[line] or "\n", col)
  local target = nil

  if forward then
    for i = 1, #diagnostics do
      local start = diagnostics[i].range.start
      if (start.line or 0) > current_line or
         ((start.line or 0) == current_line and (start.character or 0) > current_char) then
        target = diagnostics[i]
        break
      end
    end
    target = target or diagnostics[1]
  else
    for i = #diagnostics, 1, -1 do
      local start = diagnostics[i].range.start
      if (start.line or 0) < current_line or
         ((start.line or 0) == current_line and (start.character or 0) < current_char) then
        target = diagnostics[i]
        break
      end
    end
    target = target or diagnostics[#diagnostics]
  end

  open_location(path_to_uri(view.doc.abs_filename), target.range)
end

function manager.next_diagnostic()
  goto_diagnostic(true)
end

function manager.previous_diagnostic()
  goto_diagnostic(false)
end

function manager.refresh_semantic_highlighting()
  local view = current_docview()
  if not view or not view.doc.abs_filename then
    return
  end
  local client = manager.open_doc(view.doc)
  if not client then
    core.warn("No LSP server configured for %s", view.doc:get_name())
    return
  end
  manager.request_semantic_tokens(view.doc)
end

local function completion_items_to_autocomplete(result)
  local items = result and result.items or result or {}
  local out = {
    name = "lsp",
    items = {},
  }

  for _, item in ipairs(items) do
    local insert_text = item.insertText or (item.textEdit and item.textEdit.newText) or item.label
    if item.label ~= nil then
      out.items[tostring(item.label)] = {
        info = item.detail ~= nil and tostring(item.detail) or nil,
        desc = content_to_text(item.documentation),
        icon = protocol.completion_kinds[item.kind] or "keyword",
        data = item,
        onselect = function(_, selected)
          local view = current_docview()
          local doc = view and view.doc
          if not doc then
            return false
          end
        local selected_item = selected.data
        if selected_item.textEdit then
          apply_text_edit(doc, selected_item.textEdit, true)
          if selected_item.additionalTextEdits then
            local edits = {}
            for _, edit in ipairs(selected_item.additionalTextEdits) do
              edits[#edits + 1] = edit
              end
              table.sort(edits, range_sort_desc)
              for _, edit in ipairs(edits) do
                apply_text_edit(doc, edit)
              end
            end
            return true
          end
          selected.text = tostring(insert_text)
          return false
        end,
      }
    end
  end

  return out
end

autocomplete.register_provider("lsp", function(ctx, respond)
  if not ctx or not ctx.doc or not ctx.doc.abs_filename then
    return {}
  end
  local client = manager.open_doc(ctx.doc)
  if not client then
    return {}
  end
  client:request(
    "textDocument/completion",
    manager.document_params(ctx.doc, ctx.line, ctx.col),
    function(result, err)
      if err then
        core.warn("LSP completion failed: %s", err.message or tostring(err))
        respond({})
        return
      end
      respond(completion_items_to_autocomplete(result))
    end
  )
end)
autocomplete.set_default_mode("lsp")

function manager.complete()
  local view = current_docview()
  if not view or not view.doc.abs_filename then
    return
  end
  local client = manager.open_doc(view.doc)
  if not client then
    core.warn("No LSP server configured for %s", view.doc:get_name())
    return
  end

  client:request("textDocument/completion", manager.document_params(view.doc), function(result, err)
    if err then
      core.warn("LSP completion failed: %s", err.message or tostring(err))
      return
    end
    autocomplete.complete(completion_items_to_autocomplete(result))
  end)
end

function manager.rename_symbol()
  local view = current_docview()
  if not view or not view.doc.abs_filename then
    return
  end
  local client = manager.open_doc(view.doc)
  if not client then
    core.warn("No LSP server configured for %s", view.doc:get_name())
    return
  end

  local line1, col1, line2, col2 = view.doc:get_selection(true)
  local current_name = view.doc:get_text(line1, col1, line2, col2)
  core.command_view:enter("Rename symbol to", {
    text = current_name,
    select_text = true,
    suggest = function(text)
      core.status_view:show_tooltip(string.format("%s -> %s", current_name, text))
      return {}
    end,
    submit = function(text)
      core.status_view:remove_tooltip()
      if text == "" then
        return
      end
      local params = manager.document_params(view.doc)
      params.newName = text
      client:request("textDocument/rename", params, function(result, err)
        if err then
          core.warn("LSP rename failed: %s", err.message or tostring(err))
          return
        end
        apply_workspace_edit(result)
      end)
    end,
    cancel = function()
      core.status_view:remove_tooltip()
    end,
  })
end

function manager.restart()
  for _, client in pairs(manager.clients) do
    client:shutdown()
  end
  manager.reload_config()
  for _, doc in ipairs(core.docs) do
    if doc.abs_filename then
      manager.open_doc(doc)
    end
  end
end

command.add(function()
  local view = current_docview()
  return view and view.doc and view.doc.abs_filename, view
end, {
  ["lsp:next-diagnostic"] = function()
    manager.next_diagnostic()
  end,
  ["lsp:previous-diagnostic"] = function()
    manager.previous_diagnostic()
  end,
  ["lsp:code-action"] = function()
    manager.code_action()
  end,
  ["lsp:signature-help"] = function()
    manager.signature_help()
  end,
  ["lsp:format-document"] = function()
    manager.format_document()
  end,
  ["lsp:format-selection"] = function()
    manager.format_selection()
  end,
  ["lsp:hover"] = function()
    manager.hover()
  end,
  ["lsp:complete"] = function()
    manager.complete()
  end,
  ["lsp:rename-symbol"] = function()
    manager.rename_symbol()
  end,
  ["lsp:show-diagnostics"] = function()
    manager.show_diagnostics()
  end,
  ["lsp:refresh-semantic-highlighting"] = function()
    manager.refresh_semantic_highlighting()
  end,
  ["lsp:workspace-symbols"] = function()
    manager.workspace_symbols()
  end,
})

local function lsp_navigation_predicate(capability)
  return function()
    local view = current_docview()
    if not (view and view.doc and view.doc.abs_filename) then
      return false
    end
    if not manager.find_spec_for_doc(view.doc) then
      return false
    end
    local state = manager.doc_state[view.doc]
    local client = state and state.client
    if client and client.is_initialized and capability and not capability_supported(client, capability) then
      return false
    end
    return true, view
  end
end

command.add(lsp_navigation_predicate("definitionProvider"), {
  ["lsp:goto-definition"] = function()
    manager.goto_definition()
  end,
})

command.add(lsp_navigation_predicate("typeDefinitionProvider"), {
  ["lsp:goto-type-definition"] = function()
    manager.goto_type_definition()
  end,
})

command.add(lsp_navigation_predicate("implementationProvider"), {
  ["lsp:goto-implementation"] = function()
    manager.goto_implementation()
  end,
})

command.add(lsp_navigation_predicate("referencesProvider"), {
  ["lsp:find-references"] = function()
    manager.find_references()
  end,
})

command.add(lsp_navigation_predicate("documentSymbolProvider"), {
  ["lsp:show-document-symbols"] = function()
    manager.show_document_symbols()
  end,
})

command.add(lsp_navigation_predicate("codeActionProvider"), {
  ["lsp:code-action"] = function()
    manager.code_action()
  end,
  ["lsp:quick-fix"] = function()
    manager.quick_fix()
  end,
})

command.add(nil, {
  ["lsp:jump-back"] = function()
    return manager.jump_back()
  end,
  ["lsp:restart"] = function()
    manager.restart()
  end,
})

return manager
"#;

fn register_source(
    lua: &Lua,
    preload: &LuaTable,
    module_name: &'static str,
    source: &'static str,
) -> LuaResult<()> {
    preload.set(
        module_name,
        lua.create_function(move |lua, ()| lua.load(source).set_name(module_name).eval::<LuaValue>())?,
    )?;
    Ok(())
}

/// Registers all LSP plugin modules as Rust-owned preloads.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let package: LuaTable = lua.globals().get("package")?;
    let preload: LuaTable = package.get("preload")?;
    register_source(lua, &preload, "plugins.lsp", INIT_SOURCE)?;
    register_source(lua, &preload, "plugins.lsp.client", CLIENT_SOURCE)?;
    register_source(lua, &preload, "plugins.lsp.json", JSON_SOURCE)?;
    register_source(lua, &preload, "plugins.lsp.protocol", PROTOCOL_SOURCE)?;
    register_source(lua, &preload, "plugins.lsp.server-manager", LSP_MANAGER_SOURCE)?;
    Ok(())
}
