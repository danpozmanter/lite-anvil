use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use crate::editor::lsp;

/// An inlay hint from the LSP.
pub(crate) struct InlayHint {
    pub line: usize,  // 0-based
    pub col: usize,   // 0-based
    pub label: String,
}

/// A single LSP diagnostic with pre-extracted fields.
#[allow(dead_code)] // end_line and message are stored for future tooltip/multi-line support
pub(crate) struct Diagnostic {
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
    /// 1=error, 2=warning, 3=info, 4=hint
    pub severity: u8,
    pub message: String,
}

/// LSP connection state tracked in the main loop.
pub(crate) struct LspState {
    pub transport_id: Option<u64>,
    pub initialized: bool,
    pub diagnostics: HashMap<String, Vec<Diagnostic>>,
    pub pending_requests: HashMap<i64, String>,
    pub next_request_id: i64,
    pub root_uri: String,
    pub filetype: String,
    pub last_change: Option<Instant>,
    pub pending_change_uri: Option<String>,
    pub pending_change_version: i64,
    pub inlay_hints: Vec<InlayHint>,
    pub inlay_retry_at: Option<Instant>,
    pub inlay_retry_count: u32,
}

impl LspState {
    pub fn new() -> Self {
        Self {
            transport_id: None,
            initialized: false,
            diagnostics: HashMap::new(),
            pending_requests: HashMap::new(),
            next_request_id: 1,
            root_uri: String::new(),
            filetype: String::new(),
            last_change: None,
            pending_change_uri: None,
            pending_change_version: 0,
            inlay_hints: Vec::new(),
            inlay_retry_at: None,
            inlay_retry_count: 0,
        }
    }

    pub fn next_id(&mut self) -> i64 {
        let id = self.next_request_id;
        self.next_request_id += 1;
        id
    }
}

/// Autocomplete popup state for LSP completions.
pub(crate) struct CompletionState {
    pub items: Vec<(String, String, String)>,
    pub visible: bool,
    pub selected: usize,
    pub line: usize,
    pub col: usize,
}

impl CompletionState {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            visible: false,
            selected: 0,
            line: 0,
            col: 0,
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
pub(crate) fn lsp_completion_request(id: i64, uri: &str, line: usize, character: usize) -> serde_json::Value {
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
pub(crate) fn lsp_hover_request(id: i64, uri: &str, line: usize, character: usize) -> serde_json::Value {
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
pub(crate) fn lsp_definition_request(id: i64, uri: &str, line: usize, character: usize) -> serde_json::Value {
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
pub(crate) fn lsp_position_request(id: i64, method: &str, uri: &str, line: usize, character: usize) -> serde_json::Value {
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

/// Map a file extension to an LSP filetype name.
pub(crate) fn ext_to_lsp_filetype(ext: &str) -> Option<&'static str> {
    match ext {
        "rs" => Some("rust"),
        "py" | "pyw" => Some("python"),
        "js" | "mjs" | "cjs" => Some("javascript"),
        "ts" | "mts" | "cts" => Some("typescript"),
        "tsx" => Some("tsx"),
        "jsx" => Some("javascript"),
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
        _ => None,
    }
}

/// Find an LSP spec that covers the given filetype.
pub(crate) fn find_lsp_spec<'a>(filetype: &str, specs: &'a [lsp::LspSpec]) -> Option<&'a lsp::LspSpec> {
    specs.iter().find(|s| s.filetypes.iter().any(|ft| ft == filetype))
}

/// Check if any root pattern file exists in `dir` or its ancestors.
pub(crate) fn find_project_root(dir: &str, root_patterns: &[String]) -> Option<String> {
    let mut path = PathBuf::from(dir);
    loop {
        for pattern in root_patterns {
            if path.join(pattern).exists() {
                return Some(path.to_string_lossy().to_string());
            }
        }
        if !path.pop() {
            break;
        }
    }
    None
}

/// Build the LSP `initialize` request.
pub(crate) fn lsp_initialize_request(id: i64, root_uri: &str) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "processId": std::process::id(),
            "rootUri": root_uri,
            "capabilities": {
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
                    }
                }
            }
        }
    })
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

/// Build a `textDocument/inlayHint` request.
pub(crate) fn lsp_inlay_hint_request(id: i64, uri: &str, start_line: usize, end_line: usize) -> serde_json::Value {
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
