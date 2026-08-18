use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Initial respawn backoff after the first consecutive spawn failure.
const RESPAWN_BACKOFF_BASE_MS: u64 = 250;
/// Upper bound on respawn backoff so a crash-looping server is retried
/// at a steady, bounded cadence rather than ever-growing delays.
const RESPAWN_BACKOFF_CAP_MS: u64 = 30_000;

use crate::editor::lsp;

/// An inlay hint from the LSP.
pub(crate) struct InlayHint {
    pub line: usize, // 0-based
    pub col: usize,  // 0-based
    pub label: String,
}

/// A single LSP diagnostic with pre-extracted fields.
#[derive(Clone)]
pub(crate) struct Diagnostic {
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
    /// 1=error, 2=warning, 3=info, 4=hint
    pub severity: u8,
    /// Diagnostic message body shown as the mouse-hover tooltip.
    pub message: String,
    /// The original protocol value. Code-action contexts must return the
    /// diagnostic unchanged, including server-specific `code` and `data`.
    pub raw: serde_json::Value,
}

impl Diagnostic {
    pub fn from_lsp(raw: &serde_json::Value) -> Self {
        let range = raw.get("range");
        let position = |edge: &str, field: &str| {
            range
                .and_then(|r| r.get(edge))
                .and_then(|p| p.get(field))
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize
        };
        Self {
            start_line: position("start", "line"),
            start_col: position("start", "character"),
            end_line: position("end", "line"),
            end_col: position("end", "character"),
            severity: raw.get("severity").and_then(|v| v.as_u64()).unwrap_or(1) as u8,
            message: raw
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            raw: raw.clone(),
        }
    }
}

/// LSP connection state tracked in the main loop.
pub(crate) struct LspState {
    pub transport_id: Option<u64>,
    pub initialized: bool,
    /// Combined diagnostics consumed by rendering, hover, and code actions.
    pub diagnostics: HashMap<String, Vec<Diagnostic>>,
    /// Diagnostics delivered through `textDocument/publishDiagnostics`.
    pub push_diagnostics: HashMap<String, Vec<Diagnostic>>,
    /// Diagnostics delivered through `textDocument/diagnostic` responses.
    pub pull_diagnostics_by_path: HashMap<String, Vec<Diagnostic>>,
    pub pending_requests: HashMap<i64, String>,
    /// Per-request URI for pending inlayHint requests, so that late
    /// responses for a non-active file can be discarded instead of
    /// overwriting the hints currently on screen.
    pub pending_request_uris: HashMap<i64, String>,
    /// Buffer `change_id` a pending formatting request was computed against.
    /// The server's edit positions address that revision, so a response is only
    /// safe to splice while the buffer still holds it.
    pub pending_request_change_ids: HashMap<i64, i64>,
    pub next_request_id: i64,
    pub root_uri: String,
    pub filetype: String,
    /// Whether the active server can fill lazy code actions through
    /// `codeAction/resolve`.
    pub code_action_resolve_provider: bool,
    /// Whether pull diagnostics have not been explicitly rejected by the server.
    /// Some servers implement them without advertising `diagnosticProvider`.
    pub pull_diagnostics: bool,
    /// Last pull-diagnostic result id per document, used for incremental pulls.
    pub diagnostic_result_ids: HashMap<String, String>,
    /// Document URIs already announced to the active server with `didOpen`.
    pub opened_documents: HashSet<String>,
    /// Paths still to be announced, drained a few per frame. Announcing a
    /// document means joining and serializing its whole text, so opening a
    /// session's worth of tabs at once would put that work for every tab into
    /// the frame that finishes initialization.
    pub pending_did_open: std::collections::VecDeque<String>,
    /// Text last sent to each opened document. Incremental-sync servers need
    /// this to express a whole-document replacement as a valid ranged edit.
    pub document_texts: HashMap<String, String>,
    /// Whether the active server requested incremental `didChange` updates.
    pub incremental_sync: bool,
    /// Bounded retry for servers that return an empty pull while indexing.
    pub diagnostic_retry_at: Option<Instant>,
    pub diagnostic_retry_count: u8,
    pub last_change: Option<Instant>,
    pub pending_change_uri: Option<String>,
    pub pending_change_version: i64,
    pub inlay_hints: Vec<InlayHint>,
    /// URI the currently held `inlay_hints` belong to. Used to invalidate
    /// the list when the user switches to a different file.
    pub inlay_hints_uri: String,
    pub inlay_retry_at: Option<Instant>,
    pub inlay_retry_count: u32,
    /// Last buffer `change_id` observed per URI. Used to detect any
    /// buffer mutation (paste, undo, redo, snippet, format, command-driven
    /// edits, ...) regardless of which command produced it, so the
    /// debounced didChange + inlayHint re-request fires every time.
    pub last_seen_change_id: HashMap<String, i64>,
    /// Consecutive spawn/initialize failures, driving exponential respawn
    /// backoff so a crash-looping server is not relaunched every frame.
    pub respawn_failures: u32,
    /// Monotonic instant of the most recent spawn failure, gating the next
    /// respawn attempt. `None` once a spawn has succeeded.
    pub last_spawn_failure: Option<Instant>,
    /// Command used for the current initialization attempt.
    pub launch_command: Vec<String>,
    /// Commands that spawned but exited before initialization. Candidate
    /// selection skips these so any language can advance to a fallback.
    pub rejected_launch_commands: HashSet<Vec<String>>,
}

/// Probe pull diagnostics even when initialization omits `diagnosticProvider`.
/// Current Roslyn servers implement the method without advertising it. Servers
/// that return the standard "method not found" error are disabled afterward.
pub(crate) fn should_probe_pull_diagnostics() -> bool {
    true
}

/// Only the standard JSON-RPC method-not-found response proves that a server
/// cannot handle pull diagnostics. Other errors may be transient while a
/// workspace is loading and should remain eligible for retry.
pub(crate) fn pull_diagnostics_are_unsupported(response: &serde_json::Value) -> bool {
    response
        .pointer("/error/code")
        .and_then(serde_json::Value::as_i64)
        == Some(-32601)
}

impl LspState {
    pub fn new() -> Self {
        Self {
            transport_id: None,
            initialized: false,
            diagnostics: HashMap::new(),
            push_diagnostics: HashMap::new(),
            pull_diagnostics_by_path: HashMap::new(),
            pending_requests: HashMap::new(),
            pending_request_uris: HashMap::new(),
            pending_request_change_ids: HashMap::new(),
            next_request_id: 1,
            root_uri: String::new(),
            filetype: String::new(),
            code_action_resolve_provider: false,
            pull_diagnostics: false,
            diagnostic_result_ids: HashMap::new(),
            opened_documents: HashSet::new(),
            pending_did_open: std::collections::VecDeque::new(),
            document_texts: HashMap::new(),
            incremental_sync: false,
            diagnostic_retry_at: None,
            diagnostic_retry_count: 0,
            last_change: None,
            pending_change_uri: None,
            pending_change_version: 0,
            inlay_hints: Vec::new(),
            inlay_hints_uri: String::new(),
            inlay_retry_at: None,
            inlay_retry_count: 0,
            last_seen_change_id: HashMap::new(),
            respawn_failures: 0,
            last_spawn_failure: None,
            launch_command: Vec::new(),
            rejected_launch_commands: HashSet::new(),
        }
    }

    pub fn next_id(&mut self) -> i64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        id
    }

    pub fn update_push_diagnostics(&mut self, path: String, diagnostics: Vec<Diagnostic>) {
        replace_diagnostic_source(&mut self.push_diagnostics, &path, diagnostics);
        self.rebuild_diagnostics(&path);
    }

    pub fn update_pull_diagnostics(&mut self, path: String, diagnostics: Vec<Diagnostic>) {
        replace_diagnostic_source(&mut self.pull_diagnostics_by_path, &path, diagnostics);
        self.rebuild_diagnostics(&path);
    }

    fn rebuild_diagnostics(&mut self, path: &str) {
        let mut combined = Vec::new();
        for diagnostic in self.push_diagnostics.get(path).into_iter().flatten().chain(
            self.pull_diagnostics_by_path
                .get(path)
                .into_iter()
                .flatten(),
        ) {
            if !combined
                .iter()
                .any(|existing| same_diagnostic(existing, diagnostic))
            {
                combined.push(diagnostic.clone());
            }
        }
        if combined.is_empty() {
            self.diagnostics.remove(path);
        } else {
            self.diagnostics.insert(path.to_string(), combined);
        }
    }

    /// Backoff delay required before the next respawn at the current failure level.
    fn respawn_backoff(&self) -> Duration {
        if self.respawn_failures == 0 {
            return Duration::ZERO;
        }
        // 250ms, 500ms, 1s, 2s, ... doubling per failure, capped.
        let shift = (self.respawn_failures - 1).min(20);
        let ms = RESPAWN_BACKOFF_BASE_MS
            .saturating_mul(1u64 << shift)
            .min(RESPAWN_BACKOFF_CAP_MS);
        Duration::from_millis(ms)
    }

    /// Whether enough monotonic time has elapsed to retry spawning the server.
    pub fn should_attempt_spawn(&self) -> bool {
        match self.last_spawn_failure {
            None => true,
            Some(at) => at.elapsed() >= self.respawn_backoff(),
        }
    }

    /// Record a failed spawn/initialize: raise the backoff level and stamp the time.
    pub fn note_spawn_failure(&mut self) {
        self.respawn_failures = self.respawn_failures.saturating_add(1);
        self.last_spawn_failure = Some(Instant::now());
    }

    /// Drop everything that mirrored the previous server's view of the session:
    /// requests it will never answer, and the documents, texts, and result ids
    /// it tracked. A replacement server starts from a clean slate, so the
    /// editor sends `didOpen` again rather than assuming the new process
    /// already holds the documents.
    pub fn forget_server_state(&mut self) {
        self.pending_requests.clear();
        self.pending_request_uris.clear();
        self.pending_request_change_ids.clear();
        self.opened_documents.clear();
        self.pending_did_open.clear();
        self.document_texts.clear();
        self.diagnostic_result_ids.clear();
    }

    /// Record a successful initialize: clear the backoff so future spawns are immediate.
    pub fn note_spawn_success(&mut self) {
        self.respawn_failures = 0;
        self.last_spawn_failure = None;
        self.rejected_launch_commands.clear();
    }
}

fn replace_diagnostic_source(
    source: &mut HashMap<String, Vec<Diagnostic>>,
    path: &str,
    diagnostics: Vec<Diagnostic>,
) {
    if diagnostics.is_empty() {
        source.remove(path);
    } else {
        source.insert(path.to_string(), diagnostics);
    }
}

fn same_diagnostic(left: &Diagnostic, right: &Diagnostic) -> bool {
    left.start_line == right.start_line
        && left.start_col == right.start_col
        && left.end_line == right.end_line
        && left.end_col == right.end_col
        && left.severity == right.severity
        && left.message == right.message
}

/// Autocomplete popup state for LSP completions.
pub(crate) struct CompletionState {
    pub items: Vec<(String, String, String)>,
    pub visible: bool,
    pub selected: usize,
    pub line: usize,
    pub col: usize,
    /// `id` of the most recently-sent `textDocument/completion`
    /// request. Earlier responses are ignored so a slow earlier
    /// reply can't clobber a fresher one (LSP responses are not
    /// ordered against the request stream).
    pub latest_request_id: i64,
}

impl CompletionState {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            visible: false,
            selected: 0,
            line: 0,
            col: 0,
            latest_request_id: 0,
        }
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.items.clear();
        self.selected = 0;
    }
}

/// Hover tooltip state for LSP hover info.
pub(crate) struct HoverState {
    pub text: String,
    pub visible: bool,
    pub line: usize,
    pub col: usize,
}

impl HoverState {
    pub fn new() -> Self {
        Self {
            text: String::new(),
            visible: false,
            line: 0,
            col: 0,
        }
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.text.clear();
    }
}

/// Build a `textDocument/completion` request.
pub(crate) fn lsp_completion_request(
    id: i64,
    uri: &str,
    line: usize,
    character: usize,
) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "textDocument/completion",
        "params": {
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        }
    })
}

/// Build a `textDocument/hover` request.
pub(crate) fn lsp_hover_request(
    id: i64,
    uri: &str,
    line: usize,
    character: usize,
) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "textDocument/hover",
        "params": {
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        }
    })
}

/// Build a `textDocument/definition` request.
pub(crate) fn lsp_definition_request(
    id: i64,
    uri: &str,
    line: usize,
    character: usize,
) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "textDocument/definition",
        "params": {
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        }
    })
}

/// Generic LSP position request (works for definition, implementation, typeDefinition, references).
pub(crate) fn lsp_position_request(
    id: i64,
    method: &str,
    uri: &str,
    line: usize,
    character: usize,
) -> serde_json::Value {
    let mut params = serde_json::json!({
        "textDocument": { "uri": uri },
        "position": { "line": line, "character": character }
    });
    // references needs context.includeDeclaration
    if method == "textDocument/references" {
        params["context"] = serde_json::json!({ "includeDeclaration": true });
    }
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params
    })
}

/// Build a `textDocument/formatting` request.
pub(crate) fn lsp_formatting_request(
    id: i64,
    uri: &str,
    tab_size: usize,
    insert_spaces: bool,
) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "textDocument/formatting",
        "params": {
            "textDocument": { "uri": uri },
            "options": { "tabSize": tab_size, "insertSpaces": insert_spaces }
        }
    })
}

/// Build a `textDocument/rename` request.
pub(crate) fn lsp_rename_request(
    id: i64,
    uri: &str,
    line: usize,
    character: usize,
    new_name: &str,
) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "textDocument/rename",
        "params": {
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
            "newName": new_name
        }
    })
}

/// Build a `textDocument/signatureHelp` request.
pub(crate) fn lsp_signature_help_request(
    id: i64,
    uri: &str,
    line: usize,
    character: usize,
) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "textDocument/signatureHelp",
        "params": {
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character }
        }
    })
}

/// Build a `textDocument/codeAction` request.
pub(crate) fn lsp_code_action_request(
    id: i64,
    uri: &str,
    start_line: usize,
    start_char: usize,
    end_line: usize,
    end_char: usize,
    diagnostics: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "textDocument/codeAction",
        "params": {
            "textDocument": { "uri": uri },
            "range": {
                "start": { "line": start_line, "character": start_char },
                "end": { "line": end_line, "character": end_char }
            },
            "context": {
                "diagnostics": diagnostics,
                "triggerKind": 1
            }
        }
    })
}

/// Resolve `workspace/configuration` items from the active server settings.
/// An unset section must be JSON null. Returning an empty object changes the
/// meaning of scalar settings in servers such as Roslyn.
pub(crate) fn lsp_configuration_values(
    items: Option<&serde_json::Value>,
    settings: Option<&serde_json::Value>,
) -> Vec<serde_json::Value> {
    let Some(items) = items.and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .map(|item| {
            let Some(section) = item.get("section").and_then(serde_json::Value::as_str) else {
                return serde_json::Value::Null;
            };
            let Some(settings) = settings else {
                return serde_json::Value::Null;
            };
            if let Some(value) = settings.get(section) {
                return value.clone();
            }
            section
                .split('.')
                .try_fold(settings, |value, key| value.get(key))
                .cloned()
                .unwrap_or(serde_json::Value::Null)
        })
        .collect()
}

/// Build a `codeAction/resolve` request for a lazy action returned by a server.
pub(crate) fn lsp_code_action_resolve_request(
    id: i64,
    action: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "codeAction/resolve",
        "params": action
    })
}

/// Convert the editor's 1-based anchor/cursor selection to a normalized,
/// 0-based LSP range.
pub(crate) fn normalized_lsp_range(
    line1: usize,
    col1: usize,
    line2: usize,
    col2: usize,
) -> (usize, usize, usize, usize) {
    let start = (line1.saturating_sub(1), col1.saturating_sub(1));
    let end = (line2.saturating_sub(1), col2.saturating_sub(1));
    if start <= end {
        (start.0, start.1, end.0, end.1)
    } else {
        (end.0, end.1, start.0, start.1)
    }
}

/// Return published diagnostics whose lines overlap an action request range.
/// The original JSON values are retained because servers may use `code`,
/// `source`, or `data` to identify the available fixes.
pub(crate) fn code_action_diagnostics(
    diagnostics: Option<&Vec<Diagnostic>>,
    start_line: usize,
    start_col: usize,
    end_line: usize,
    end_col: usize,
) -> serde_json::Value {
    serde_json::Value::Array(
        diagnostics
            .into_iter()
            .flatten()
            .filter(|d| {
                (d.start_line, d.start_col) <= (end_line, end_col)
                    && (d.end_line, d.end_col.max(d.start_col + 1)) >= (start_line, start_col)
            })
            .map(|d| d.raw.clone())
            .collect(),
    )
}

/// Map a file extension to an LSP filetype name.
pub(crate) fn ext_to_lsp_filetype(ext: &str) -> Option<&'static str> {
    match ext {
        "rs" => Some("rust"),
        "py" | "pyw" => Some("python"),
        "js" | "mjs" | "cjs" => Some("javascript"),
        "ts" | "mts" | "cts" => Some("typescript"),
        "tsx" => Some("tsx"),
        "jsx" => Some("jsx"),
        "go" => Some("go"),
        "c" | "h" => Some("c"),
        "cpp" | "cc" | "cxx" | "hpp" => Some("c++"),
        "java" => Some("java"),
        "kt" | "kts" => Some("kotlin"),
        "lua" => Some("lua"),
        "rb" => Some("ruby"),
        "php" => Some("php"),
        "ex" | "exs" => Some("elixir"),
        "ml" | "mli" => Some("ocaml"),
        "gleam" => Some("gleam"),
        "erl" | "hrl" => Some("erlang"),
        "hs" => Some("haskell"),
        "zig" => Some("zig"),
        "cs" => Some("c#"),
        "fs" | "fsi" | "fsx" => Some("f#"),
        "svelte" => Some("svelte"),
        "gos" => Some("gossamer"),
        "dart" => Some("dart"),
        "scala" | "sc" => Some("scala"),
        "swift" => Some("swift"),
        "jl" => Some("julia"),
        "clj" | "cljc" | "cljs" | "edn" => Some("clojure"),
        "cr" => Some("crystal"),
        "sh" | "bash" | "zsh" => Some("bash"),
        _ => None,
    }
}

/// Map the editor's internal filetype name to the language identifier sent in
/// `textDocument/didOpen`.
pub(crate) fn lsp_language_id(filetype: &str) -> &str {
    match filetype {
        "c#" => "csharp",
        "c++" => "cpp",
        "f#" => "fsharp",
        "jsx" => "javascriptreact",
        "tsx" => "typescriptreact",
        "bash" => "shellscript",
        other => other,
    }
}

/// Find an LSP spec that covers the given filetype.
pub(crate) fn find_lsp_spec<'a>(
    filetype: &str,
    specs: &'a [lsp::LspSpec],
) -> Option<&'a lsp::LspSpec> {
    specs
        .iter()
        .find(|s| s.filetypes.iter().any(|ft| ft == filetype))
}

/// Check if any root pattern file exists in `dir` or its ancestors.
pub(crate) fn find_project_root(dir: &str, root_patterns: &[String]) -> Option<String> {
    let mut path = PathBuf::from(dir);
    loop {
        for pattern in root_patterns {
            if root_pattern_exists(&path, pattern) {
                return Some(path.to_string_lossy().to_string());
            }
        }
        if !path.pop() {
            break;
        }
    }
    None
}

fn root_pattern_exists(dir: &std::path::Path, pattern: &str) -> bool {
    if dir.join(pattern).exists() {
        return true;
    }
    let suffix = pattern
        .strip_prefix('*')
        .or_else(|| pattern.starts_with('.').then_some(pattern));
    let Some(suffix) = suffix else { return false };
    std::fs::read_dir(dir).ok().is_some_and(|entries| {
        entries.flatten().any(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.ends_with(suffix))
        })
    })
}

/// Build the LSP `initialize` request.
#[cfg(test)]
pub(crate) fn lsp_initialize_request(id: i64, root_uri: &str) -> serde_json::Value {
    lsp_initialize_request_with_options(id, root_uri, None)
}

pub(crate) fn lsp_initialize_request_with_options(
    id: i64,
    root_uri: &str,
    initialization_options: Option<&serde_json::Value>,
) -> serde_json::Value {
    let mut request = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "processId": std::process::id(),
            "rootUri": root_uri,
            "workspaceFolders": [{ "uri": root_uri, "name": "workspace" }],
            "capabilities": {
                "workspace": {
                    "applyEdit": true,
                    "configuration": true,
                    "workspaceFolders": true,
                    "workspaceEdit": {
                        "documentChanges": true
                    },
                    "diagnostics": {
                        "refreshSupport": true
                    }
                },
                "window": {
                    "workDoneProgress": true
                },
                "textDocument": {
                    "publishDiagnostics": { "relatedInformation": true },
                    "synchronization": {
                        "didSave": true,
                        "dynamicRegistration": false
                    },
                    "completion": {
                        "completionItem": { "snippetSupport": false }
                    },
                    "hover": { "contentFormat": ["plaintext"] },
                    "definition": {},
                    "implementation": {},
                    "typeDefinition": {},
                    "references": {},
                    "inlayHint": {
                        "dynamicRegistration": false
                    },
                    "formatting": {},
                    "rename": { "prepareSupport": false },
                    "signatureHelp": {
                        "signatureInformation": {
                            "documentationFormat": ["plaintext"]
                        }
                    },
                    "codeAction": {
                        "dataSupport": true,
                        "isPreferredSupport": true,
                        "disabledSupport": true,
                        "resolveSupport": {
                            "properties": ["edit", "command"]
                        },
                        "codeActionLiteralSupport": {
                            "codeActionKind": {
                                "valueSet": [
                                    "quickfix",
                                    "refactor",
                                    "refactor.extract",
                                    "refactor.inline",
                                    "refactor.rewrite",
                                    "source",
                                    "source.organizeImports",
                                    "source.fixAll"
                                ]
                            }
                        }
                    },
                    "diagnostic": {
                        "dynamicRegistration": true,
                        "relatedDocumentSupport": false
                    }
                }
            }
        }
    });
    if let Some(options) = initialization_options {
        request["params"]["initializationOptions"] = options.clone();
    }
    request
}

/// Build an LSP 3.17 pull-diagnostic request.
pub(crate) fn lsp_document_diagnostic_request(
    id: i64,
    uri: &str,
    previous_result_id: Option<&str>,
) -> serde_json::Value {
    let mut params = serde_json::json!({
        "textDocument": { "uri": uri }
    });
    if let Some(result_id) = previous_result_id {
        params["previousResultId"] = serde_json::Value::String(result_id.to_string());
    }
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "textDocument/diagnostic",
        "params": params
    })
}

/// Build the Roslyn project-loading notification used by the current VS Code
/// C# server. Standard LSP workspace folders do not cause this server to load
/// `.sln` or `.csproj` files.
pub(crate) fn csharp_project_open_notification(root_uri: &str) -> Option<serde_json::Value> {
    let root = PathBuf::from(uri_to_path(root_uri));
    let mut stack = vec![root];
    let mut solutions = Vec::new();
    let mut projects = Vec::new();
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !matches!(name.as_ref(), ".git" | "bin" | "obj" | "node_modules") {
                    stack.push(path);
                }
                continue;
            }
            match path.extension().and_then(|extension| extension.to_str()) {
                Some("sln") if solutions.len() < 2 => {
                    solutions.push(path_to_uri(&path.to_string_lossy()))
                }
                Some("csproj") if projects.len() < 100 => {
                    projects.push(path_to_uri(&path.to_string_lossy()))
                }
                _ => {}
            }
        }
    }
    solutions.sort();
    projects.sort();
    if solutions.len() == 1 {
        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "solution/open",
            "params": { "solution": solutions[0] }
        }))
    } else if !projects.is_empty() {
        Some(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "project/open",
            "params": { "projects": projects }
        }))
    } else {
        None
    }
}

/// Build a `textDocument/didOpen` notification.
pub(crate) fn lsp_did_open(uri: &str, language_id: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didOpen",
        "params": {
            "textDocument": {
                "uri": uri,
                "languageId": language_id,
                "version": 1,
                "text": text
            }
        }
    })
}

/// Build a `textDocument/didSave` notification.
pub(crate) fn lsp_did_save(uri: &str) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didSave",
        "params": {
            "textDocument": { "uri": uri }
        }
    })
}

/// Build a `textDocument/didChange` notification (full sync).
pub(crate) fn lsp_did_change(uri: &str, version: i64, text: &str) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": uri, "version": version },
            "contentChanges": [{ "text": text }]
        }
    })
}

/// Build an incremental `didChange` notification that replaces the previous
/// document contents. This remains a single update while satisfying servers
/// that reject full-sync payloads without a range.
pub(crate) fn lsp_incremental_did_change(
    uri: &str,
    version: i64,
    previous_text: &str,
    text: &str,
) -> serde_json::Value {
    let (end_line, end_character) = lsp_end_position(previous_text);
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": "textDocument/didChange",
        "params": {
            "textDocument": { "uri": uri, "version": version },
            "contentChanges": [{
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": end_line, "character": end_character }
                },
                "rangeLength": previous_text.encode_utf16().count(),
                "text": text
            }]
        }
    })
}

fn lsp_end_position(text: &str) -> (usize, usize) {
    let mut line = 0;
    let mut character = 0;
    for ch in text.chars() {
        if ch == '\n' {
            line += 1;
            character = 0;
        } else {
            character += ch.len_utf16();
        }
    }
    (line, character)
}

/// Build a `textDocument/inlayHint` request.
pub(crate) fn lsp_inlay_hint_request(
    id: i64,
    uri: &str,
    start_line: usize,
    end_line: usize,
) -> serde_json::Value {
    // end_line should be 0-based last line index (line_count - 1).
    let end = if end_line > 0 { end_line - 1 } else { 0 };
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "textDocument/inlayHint",
        "params": {
            "textDocument": { "uri": uri },
            "range": {
                "start": { "line": start_line, "character": 0 },
                "end": { "line": end, "character": 0 }
            }
        }
    })
}

/// Convert a file path to a file:// URI.
pub(crate) fn path_to_uri(path: &str) -> String {
    let abs = if path.starts_with('/') {
        path.to_string()
    } else {
        std::env::current_dir()
            .map(|d| d.join(path).to_string_lossy().to_string())
            .unwrap_or_else(|_| path.to_string())
    };
    format!("file://{abs}")
}

/// Extract a file path from a file:// URI.
pub(crate) fn uri_to_path(uri: &str) -> String {
    uri.strip_prefix("file://").unwrap_or(uri).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsp_state_new_starts_uninitialized() {
        let s = LspState::new();
        assert!(s.transport_id.is_none());
        assert!(!s.initialized);
        assert!(s.diagnostics.is_empty());
        assert!(s.pending_requests.is_empty());
        assert_eq!(s.next_request_id, 1);
        assert!(s.inlay_hints.is_empty());
    }

    #[test]
    fn pull_diagnostics_are_probed_when_capability_is_missing() {
        assert!(should_probe_pull_diagnostics());
    }

    #[test]
    fn only_method_not_found_disables_pull_diagnostics() {
        assert!(pull_diagnostics_are_unsupported(&serde_json::json!({
            "error": { "code": -32601 }
        })));
        assert!(!pull_diagnostics_are_unsupported(&serde_json::json!({
            "error": { "code": -32603 }
        })));
        assert!(!pull_diagnostics_are_unsupported(&serde_json::json!({
            "result": null
        })));
    }

    #[test]
    fn forgetting_server_state_reopens_documents_on_the_next_server() {
        let mut s = LspState::new();
        s.opened_documents.insert("file:///a.gos".to_string());
        s.document_texts
            .insert("file:///a.gos".to_string(), "old".to_string());
        s.diagnostic_result_ids
            .insert("file:///a.gos".to_string(), "r1".to_string());
        s.pending_requests
            .insert(7, "textDocument/formatting".to_string());
        s.pending_request_uris
            .insert(7, "file:///a.gos".to_string());
        s.pending_request_change_ids.insert(7, 42);
        s.filetype = "gossamer".to_string();

        s.forget_server_state();

        assert!(s.opened_documents.is_empty());
        assert!(s.document_texts.is_empty());
        assert!(s.diagnostic_result_ids.is_empty());
        assert!(s.pending_requests.is_empty());
        assert!(s.pending_request_uris.is_empty());
        assert!(s.pending_request_change_ids.is_empty());
        // The (filetype, root) identity survives: it names the state, not the
        // process that happened to be serving it.
        assert_eq!(s.filetype, "gossamer");
    }

    #[test]
    fn respawn_backoff_gates_attempts() {
        let mut s = LspState::new();
        // A fresh state has no failures, so a spawn is allowed immediately.
        assert!(s.should_attempt_spawn());

        // A failure stamps the backoff window; the next attempt is gated.
        s.note_spawn_failure();
        assert_eq!(s.respawn_failures, 1);
        assert!(!s.should_attempt_spawn());

        // Backoff grows with consecutive failures and stays within the cap.
        s.note_spawn_failure();
        assert_eq!(s.respawn_failures, 2);
        assert!(s.respawn_backoff() <= Duration::from_millis(RESPAWN_BACKOFF_CAP_MS));

        // A success clears the backoff so spawns are immediate again.
        s.note_spawn_success();
        assert_eq!(s.respawn_failures, 0);
        assert!(s.last_spawn_failure.is_none());
        assert!(s.should_attempt_spawn());
    }

    #[test]
    fn respawn_backoff_is_capped() {
        let mut s = LspState::new();
        for _ in 0..40 {
            s.note_spawn_failure();
        }
        assert_eq!(
            s.respawn_backoff(),
            Duration::from_millis(RESPAWN_BACKOFF_CAP_MS)
        );
    }

    #[test]
    fn next_id_is_monotonic_and_unique() {
        let mut s = LspState::new();
        let a = s.next_id();
        let b = s.next_id();
        let c = s.next_id();
        assert_eq!(a, 1);
        assert_eq!(b, 2);
        assert_eq!(c, 3);
        assert_ne!(a, b);
    }

    #[test]
    fn pending_request_insert_and_remove() {
        // Pending-request lifecycle: callers register a method by id, then remove it on response.
        let mut s = LspState::new();
        let id = s.next_id();
        s.pending_requests
            .insert(id, "textDocument/completion".to_string());
        assert_eq!(s.pending_requests.len(), 1);

        // Simulate a response: remove by id.
        let removed = s.pending_requests.remove(&id);
        assert_eq!(removed.as_deref(), Some("textDocument/completion"));
        assert!(s.pending_requests.is_empty());
    }

    #[test]
    fn unknown_response_id_is_tolerated() {
        let mut s = LspState::new();
        // Server replies with an id that was never sent — must not panic and must not affect state.
        assert!(s.pending_requests.remove(&999).is_none());
        assert!(s.pending_requests.is_empty());
    }

    #[test]
    fn diagnostics_replace_on_new_publish() {
        let mut s = LspState::new();
        let uri = "file:///foo.rs".to_string();
        s.diagnostics.insert(
            uri.clone(),
            vec![Diagnostic {
                start_line: 1,
                start_col: 1,
                end_line: 1,
                end_col: 5,
                severity: 1,
                message: String::new(),
                raw: serde_json::Value::Null,
            }],
        );
        // New publishDiagnostics for the same URI replaces (HashMap insert overwrites).
        s.diagnostics.insert(
            uri.clone(),
            vec![Diagnostic {
                start_line: 2,
                start_col: 1,
                end_line: 2,
                end_col: 5,
                severity: 2,
                message: String::new(),
                raw: serde_json::Value::Null,
            }],
        );
        let v = &s.diagnostics[&uri];
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].start_line, 2);
        assert_eq!(v[0].severity, 2);
    }

    #[test]
    fn diagnostics_for_different_uris_are_independent() {
        let mut s = LspState::new();
        s.diagnostics.insert(
            "file:///a.rs".to_string(),
            vec![Diagnostic {
                start_line: 1,
                start_col: 1,
                end_line: 1,
                end_col: 1,
                severity: 1,
                message: String::new(),
                raw: serde_json::Value::Null,
            }],
        );
        s.diagnostics.insert(
            "file:///b.rs".to_string(),
            vec![Diagnostic {
                start_line: 5,
                start_col: 1,
                end_line: 5,
                end_col: 1,
                severity: 2,
                message: String::new(),
                raw: serde_json::Value::Null,
            }],
        );
        assert_eq!(s.diagnostics.len(), 2);
        assert_eq!(s.diagnostics["file:///a.rs"][0].severity, 1);
        assert_eq!(s.diagnostics["file:///b.rs"][0].severity, 2);
    }

    #[test]
    fn empty_pull_does_not_erase_pushed_diagnostics() {
        let mut state = LspState::new();
        let path = "/project/main.lang".to_string();
        state.update_push_diagnostics(path.clone(), vec![test_diagnostic("pushed error")]);

        state.update_pull_diagnostics(path.clone(), Vec::new());

        assert_eq!(state.diagnostics[&path].len(), 1);
        assert_eq!(state.diagnostics[&path][0].message, "pushed error");
        state.update_push_diagnostics(path.clone(), Vec::new());
        assert!(!state.diagnostics.contains_key(&path));
    }

    #[test]
    fn empty_push_does_not_erase_pulled_diagnostics() {
        let mut state = LspState::new();
        let path = "/project/main.lang".to_string();
        state.update_pull_diagnostics(path.clone(), vec![test_diagnostic("pulled error")]);

        state.update_push_diagnostics(path.clone(), Vec::new());

        assert_eq!(state.diagnostics[&path].len(), 1);
        assert_eq!(state.diagnostics[&path][0].message, "pulled error");
    }

    #[test]
    fn matching_push_and_pull_diagnostics_are_deduplicated() {
        let mut state = LspState::new();
        let path = "/project/main.lang".to_string();
        state.update_push_diagnostics(path.clone(), vec![test_diagnostic("same error")]);
        state.update_pull_diagnostics(path.clone(), vec![test_diagnostic("same error")]);

        assert_eq!(state.diagnostics[&path].len(), 1);
    }

    fn test_diagnostic(message: &str) -> Diagnostic {
        Diagnostic {
            start_line: 1,
            start_col: 2,
            end_line: 1,
            end_col: 5,
            severity: 1,
            message: message.to_string(),
            raw: serde_json::json!({
                "range": {
                    "start": { "line": 1, "character": 2 },
                    "end": { "line": 1, "character": 5 }
                },
                "severity": 1,
                "message": message
            }),
        }
    }

    #[test]
    fn completion_state_hide_clears_items_and_selection() {
        let mut c = CompletionState::new();
        c.items.push(("foo".into(), "bar".into(), "baz".into()));
        c.visible = true;
        c.selected = 1;
        c.hide();
        assert!(c.items.is_empty());
        assert!(!c.visible);
        assert_eq!(c.selected, 0);
    }

    #[test]
    fn hover_state_hide_clears_text() {
        let mut h = HoverState::new();
        h.text = "tooltip body".to_string();
        h.visible = true;
        h.hide();
        assert!(h.text.is_empty());
        assert!(!h.visible);
    }

    #[test]
    fn lsp_completion_request_shape() {
        let req = lsp_completion_request(7, "file:///x.rs", 10, 5);
        assert_eq!(req["jsonrpc"], "2.0");
        assert_eq!(req["id"], 7);
        assert_eq!(req["method"], "textDocument/completion");
        assert_eq!(req["params"]["textDocument"]["uri"], "file:///x.rs");
        assert_eq!(req["params"]["position"]["line"], 10);
        assert_eq!(req["params"]["position"]["character"], 5);
    }

    #[test]
    fn lsp_position_request_for_references_includes_declaration() {
        let req = lsp_position_request(3, "textDocument/references", "file:///x.rs", 1, 2);
        assert_eq!(
            req["params"]["context"]["includeDeclaration"],
            serde_json::Value::Bool(true)
        );
    }

    #[test]
    fn lsp_position_request_for_definition_omits_context() {
        let req = lsp_position_request(3, "textDocument/definition", "file:///x.rs", 1, 2);
        // Definition requests must NOT include the references-only `context` field.
        assert!(req["params"].get("context").is_none());
    }

    #[test]
    fn lsp_initialize_request_includes_capabilities() {
        let req = lsp_initialize_request(1, "file:///root");
        assert_eq!(req["method"], "initialize");
        assert_eq!(req["params"]["rootUri"], "file:///root");
        assert!(req["params"]["capabilities"]["textDocument"]["completion"].is_object());
        assert!(req["params"]["capabilities"]["textDocument"]["hover"].is_object());
        assert!(req["params"]["capabilities"]["textDocument"]["inlayHint"].is_object());
        assert_eq!(
            req["params"]["capabilities"]["workspace"]["applyEdit"],
            true
        );
        assert_eq!(
            req["params"]["capabilities"]["workspace"]["workspaceEdit"]["documentChanges"],
            true
        );
        assert_eq!(
            req["params"]["capabilities"]["workspace"]["configuration"],
            true
        );
        assert_eq!(
            req["params"]["capabilities"]["textDocument"]["diagnostic"]["dynamicRegistration"],
            true
        );
    }

    #[test]
    fn lsp_initialize_request_advertises_new_features() {
        let req = lsp_initialize_request(1, "file:///root");
        let td = &req["params"]["capabilities"]["textDocument"];
        assert!(td["formatting"].is_object());
        assert!(td["rename"].is_object());
        assert_eq!(
            td["rename"]["prepareSupport"],
            serde_json::Value::Bool(false)
        );
        assert!(td["signatureHelp"].is_object());
        assert_eq!(
            td["signatureHelp"]["signatureInformation"]["documentationFormat"][0],
            "plaintext"
        );
        assert!(td["codeAction"].is_object());
        assert_eq!(
            td["codeAction"]["codeActionLiteralSupport"]["codeActionKind"]["valueSet"][0],
            "quickfix"
        );
        assert_eq!(td["codeAction"]["dataSupport"], true);
        assert_eq!(
            td["codeAction"]["resolveSupport"]["properties"],
            serde_json::json!(["edit", "command"])
        );
    }

    #[test]
    fn lsp_formatting_request_shape() {
        let req = lsp_formatting_request(11, "file:///x.rs", 4, true);
        assert_eq!(req["id"], 11);
        assert_eq!(req["method"], "textDocument/formatting");
        assert_eq!(req["params"]["textDocument"]["uri"], "file:///x.rs");
        assert_eq!(req["params"]["options"]["tabSize"], 4);
        assert_eq!(
            req["params"]["options"]["insertSpaces"],
            serde_json::Value::Bool(true)
        );
    }

    #[test]
    fn lsp_rename_request_shape() {
        let req = lsp_rename_request(12, "file:///x.rs", 3, 7, "renamed");
        assert_eq!(req["id"], 12);
        assert_eq!(req["method"], "textDocument/rename");
        assert_eq!(req["params"]["textDocument"]["uri"], "file:///x.rs");
        assert_eq!(req["params"]["position"]["line"], 3);
        assert_eq!(req["params"]["position"]["character"], 7);
        assert_eq!(req["params"]["newName"], "renamed");
    }

    #[test]
    fn lsp_signature_help_request_shape() {
        let req = lsp_signature_help_request(13, "file:///x.rs", 9, 2);
        assert_eq!(req["id"], 13);
        assert_eq!(req["method"], "textDocument/signatureHelp");
        assert_eq!(req["params"]["textDocument"]["uri"], "file:///x.rs");
        assert_eq!(req["params"]["position"]["line"], 9);
        assert_eq!(req["params"]["position"]["character"], 2);
    }

    #[test]
    fn lsp_code_action_request_shape() {
        let diags = serde_json::json!([{ "message": "unused" }]);
        let req = lsp_code_action_request(14, "file:///x.rs", 1, 0, 1, 8, diags);
        assert_eq!(req["id"], 14);
        assert_eq!(req["method"], "textDocument/codeAction");
        assert_eq!(req["params"]["textDocument"]["uri"], "file:///x.rs");
        assert_eq!(req["params"]["range"]["start"]["line"], 1);
        assert_eq!(req["params"]["range"]["start"]["character"], 0);
        assert_eq!(req["params"]["range"]["end"]["line"], 1);
        assert_eq!(req["params"]["range"]["end"]["character"], 8);
        assert_eq!(
            req["params"]["context"]["diagnostics"][0]["message"],
            "unused"
        );
        assert_eq!(req["params"]["context"]["triggerKind"], 1);
    }

    #[test]
    fn configuration_values_use_null_for_unset_scalar_settings() {
        let items = serde_json::json!([
            { "section": "dotnet.projects.binaryLogPath" },
            { "section": "rust-analyzer.check" }
        ]);
        assert_eq!(
            lsp_configuration_values(Some(&items), None),
            vec![serde_json::Value::Null, serde_json::Value::Null]
        );

        let settings = serde_json::json!({
            "dotnet": { "projects": { "binaryLogPath": null } },
            "rust-analyzer.check": false
        });
        assert_eq!(
            lsp_configuration_values(Some(&items), Some(&settings)),
            vec![serde_json::Value::Null, serde_json::json!(false)]
        );
    }

    #[test]
    fn lsp_code_action_resolve_request_uses_action_as_params() {
        let action = serde_json::json!({
            "title": "Import HashMap",
            "data": { "id": 7 }
        });
        let req = lsp_code_action_resolve_request(15, action.clone());
        assert_eq!(req["id"], 15);
        assert_eq!(req["method"], "codeAction/resolve");
        assert_eq!(req["params"], action);
    }

    #[test]
    fn csharp_uses_the_standard_lsp_language_id() {
        assert_eq!(lsp_language_id("c#"), "csharp");
        assert_eq!(lsp_language_id("rust"), "rust");
    }

    #[test]
    fn project_root_suffix_marker_matches_project_file() {
        let test_name = std::thread::current()
            .name()
            .unwrap_or("test")
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                    ch
                } else {
                    '_'
                }
            })
            .collect::<String>();
        let root = std::env::temp_dir().join(format!(
            "lite-anvil-lsp-root-{}-{}",
            std::process::id(),
            test_name
        ));
        let nested = root.join("src");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(root.join("Example.csproj"), "<Project />").unwrap();
        assert_eq!(
            find_project_root(nested.to_str().unwrap(), &[".csproj".to_string()]),
            Some(root.to_string_lossy().to_string())
        );
        let notification =
            csharp_project_open_notification(&path_to_uri(&root.to_string_lossy())).unwrap();
        assert_eq!(notification["method"], "project/open");
        assert_eq!(
            notification["params"]["projects"][0],
            path_to_uri(&root.join("Example.csproj").to_string_lossy())
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pull_diagnostic_request_reuses_result_id() {
        let req = lsp_document_diagnostic_request(9, "file:///tmp/a.cs", Some("result-1"));
        assert_eq!(req["method"], "textDocument/diagnostic");
        assert_eq!(req["params"]["textDocument"]["uri"], "file:///tmp/a.cs");
        assert_eq!(req["params"]["previousResultId"], "result-1");
    }

    #[test]
    fn normalized_lsp_range_orders_backwards_selection() {
        assert_eq!(normalized_lsp_range(4, 9, 2, 3), (1, 2, 3, 8));
        assert_eq!(normalized_lsp_range(2, 3, 4, 9), (1, 2, 3, 8));
    }

    #[test]
    fn code_action_diagnostics_preserve_server_fields() {
        let raw = serde_json::json!({
            "range": {
                "start": { "line": 3, "character": 4 },
                "end": { "line": 3, "character": 8 }
            },
            "severity": 2,
            "code": "E0425",
            "source": "rustc",
            "message": "cannot find value",
            "data": { "fix": 42 }
        });
        let diagnostics = vec![Diagnostic::from_lsp(&raw)];
        let context = code_action_diagnostics(Some(&diagnostics), 3, 4, 3, 8);
        assert_eq!(context[0], raw);
        assert_eq!(diagnostics[0].start_col, 4);
        assert_eq!(diagnostics[0].end_col, 8);
        assert!(
            code_action_diagnostics(Some(&diagnostics), 4, 0, 4, 20)
                .as_array()
                .is_some_and(Vec::is_empty)
        );
    }

    #[test]
    fn lsp_did_open_carries_text() {
        let req = lsp_did_open("file:///x.rs", "rust", "fn main() {}");
        assert_eq!(req["method"], "textDocument/didOpen");
        assert_eq!(req["params"]["textDocument"]["text"], "fn main() {}");
        assert_eq!(req["params"]["textDocument"]["languageId"], "rust");
        assert_eq!(req["params"]["textDocument"]["version"], 1);
        assert!(req.get("id").is_none(), "didOpen is a notification, no id");
    }

    #[test]
    fn lsp_did_change_increments_version() {
        let r1 = lsp_did_change("file:///x.rs", 1, "v1");
        let r2 = lsp_did_change("file:///x.rs", 2, "v2");
        assert_eq!(r1["params"]["textDocument"]["version"], 1);
        assert_eq!(r2["params"]["textDocument"]["version"], 2);
        assert_eq!(r2["params"]["contentChanges"][0]["text"], "v2");
    }

    #[test]
    fn incremental_did_change_replaces_the_prior_document_with_a_valid_range() {
        let request = lsp_incremental_did_change("file:///x.cs", 4, "a\nb😀", "replacement");
        let change = &request["params"]["contentChanges"][0];
        assert_eq!(request["params"]["textDocument"]["version"], 4);
        assert_eq!(change["range"]["start"]["line"], 0);
        assert_eq!(change["range"]["start"]["character"], 0);
        assert_eq!(change["range"]["end"]["line"], 1);
        assert_eq!(change["range"]["end"]["character"], 3);
        assert_eq!(change["rangeLength"], 5);
        assert_eq!(change["text"], "replacement");
    }

    #[test]
    fn lsp_inlay_hint_request_clamps_end_line() {
        let req = lsp_inlay_hint_request(1, "file:///x.rs", 0, 0);
        // end_line=0 → end becomes 0, not panicking on subtract.
        assert_eq!(req["params"]["range"]["end"]["line"], 0);
    }

    #[test]
    fn lsp_inlay_hint_request_normal_range() {
        let req = lsp_inlay_hint_request(1, "file:///x.rs", 0, 50);
        assert_eq!(req["params"]["range"]["start"]["line"], 0);
        assert_eq!(req["params"]["range"]["end"]["line"], 49);
    }

    #[test]
    fn forgetting_a_server_drops_documents_still_queued_for_announcement() {
        let mut s = LspState::new();
        s.opened_documents.insert("file:///a.gos".to_string());
        s.pending_did_open.push_back("/b.gos".to_string());
        s.forget_server_state();
        assert!(s.opened_documents.is_empty());
        assert!(
            s.pending_did_open.is_empty(),
            "a replacement server must not be sent documents queued for the old one"
        );
    }

    #[test]
    fn ext_to_lsp_filetype_known_extensions() {
        assert_eq!(ext_to_lsp_filetype("rs"), Some("rust"));
        assert_eq!(ext_to_lsp_filetype("py"), Some("python"));
        assert_eq!(ext_to_lsp_filetype("pyw"), Some("python"));
        assert_eq!(ext_to_lsp_filetype("ts"), Some("typescript"));
        assert_eq!(ext_to_lsp_filetype("tsx"), Some("tsx"));
        assert_eq!(ext_to_lsp_filetype("cpp"), Some("c++"));
        assert_eq!(ext_to_lsp_filetype("cs"), Some("c#"));
        assert_eq!(ext_to_lsp_filetype("gos"), Some("gossamer"));
        assert_eq!(ext_to_lsp_filetype("dart"), Some("dart"));
        assert_eq!(ext_to_lsp_filetype("scala"), Some("scala"));
        assert_eq!(ext_to_lsp_filetype("swift"), Some("swift"));
        assert_eq!(ext_to_lsp_filetype("rb"), Some("ruby"));
        assert_eq!(ext_to_lsp_filetype("jl"), Some("julia"));
        assert_eq!(ext_to_lsp_filetype("clj"), Some("clojure"));
        assert_eq!(ext_to_lsp_filetype("cr"), Some("crystal"));
        assert_eq!(ext_to_lsp_filetype("sh"), Some("bash"));
        assert_eq!(lsp_language_id("jsx"), "javascriptreact");
        assert_eq!(lsp_language_id("tsx"), "typescriptreact");
        assert_eq!(lsp_language_id("bash"), "shellscript");
    }

    #[test]
    fn ext_to_lsp_filetype_unknown_returns_none() {
        assert!(ext_to_lsp_filetype("xyz").is_none());
        assert!(ext_to_lsp_filetype("").is_none());
    }

    #[test]
    fn path_to_uri_absolute_path() {
        assert_eq!(path_to_uri("/usr/src/main.rs"), "file:///usr/src/main.rs");
    }

    #[test]
    fn uri_to_path_strips_scheme() {
        assert_eq!(uri_to_path("file:///usr/src/main.rs"), "/usr/src/main.rs");
    }

    #[test]
    fn uri_to_path_passthrough_when_not_file_scheme() {
        // Defensive: a non-file URI is returned unchanged rather than crashing.
        assert_eq!(uri_to_path("http://example.com/x"), "http://example.com/x");
    }

    #[test]
    fn path_uri_round_trip_absolute() {
        let p = "/tmp/test_file.rs";
        assert_eq!(uri_to_path(&path_to_uri(p)), p);
    }

    #[test]
    fn find_project_root_finds_marker_in_current_dir() {
        let tmp = std::env::temp_dir().join(format!("liteanvil_lsp_root_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("Cargo.toml"), "").unwrap();

        let root = find_project_root(tmp.to_str().unwrap(), &["Cargo.toml".to_string()]);
        assert_eq!(root.as_deref(), Some(tmp.to_str().unwrap()));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn find_project_root_walks_up_to_ancestor() {
        let tmp =
            std::env::temp_dir().join(format!("liteanvil_lsp_root_up_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let nested = tmp.join("a").join("b").join("c");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(tmp.join("Cargo.toml"), "").unwrap();

        let root = find_project_root(nested.to_str().unwrap(), &["Cargo.toml".to_string()]);
        assert_eq!(root.as_deref(), Some(tmp.to_str().unwrap()));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn find_project_root_returns_none_when_no_marker() {
        let tmp =
            std::env::temp_dir().join(format!("liteanvil_lsp_no_root_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let root = find_project_root(tmp.to_str().unwrap(), &["nonexistent_marker".to_string()]);
        // Walks up to /; on most systems there is no nonexistent_marker anywhere → None.
        assert!(root.is_none());

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
