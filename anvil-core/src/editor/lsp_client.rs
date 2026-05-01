use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use crate::editor::lsp;

/// An inlay hint from the LSP.
pub(crate) struct InlayHint {
    pub line: usize, // 0-based
    pub col: usize,  // 0-based
    pub label: String,
}

/// A single LSP diagnostic with pre-extracted fields.
pub(crate) struct Diagnostic {
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
    /// 1=error, 2=warning, 3=info, 4=hint
    pub severity: u8,
    /// Diagnostic message body shown as the mouse-hover tooltip.
    pub message: String,
}

/// LSP connection state tracked in the main loop.
pub(crate) struct LspState {
    pub transport_id: Option<u64>,
    pub initialized: bool,
    pub diagnostics: HashMap<String, Vec<Diagnostic>>,
    pub pending_requests: HashMap<i64, String>,
    /// Per-request URI for pending inlayHint requests, so that late
    /// responses for a non-active file can be discarded instead of
    /// overwriting the hints currently on screen.
    pub pending_request_uris: HashMap<i64, String>,
    pub next_request_id: i64,
    pub root_uri: String,
    pub filetype: String,
    pub last_change: Option<Instant>,
    pub pending_change_uri: Option<String>,
    pub pending_change_version: i64,
    pub inlay_hints: Vec<InlayHint>,
    /// URI the currently held `inlay_hints` belong to. Used to invalidate
    /// the list when the user switches to a different file.
    pub inlay_hints_uri: String,
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
            pending_request_uris: HashMap::new(),
            next_request_id: 1,
            root_uri: String::new(),
            filetype: String::new(),
            last_change: None,
            pending_change_uri: None,
            pending_change_version: 0,
            inlay_hints: Vec::new(),
            inlay_hints_uri: String::new(),
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
        "gos" => Some("gossamer"),
        _ => None,
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
            }],
        );
        assert_eq!(s.diagnostics.len(), 2);
        assert_eq!(s.diagnostics["file:///a.rs"][0].severity, 1);
        assert_eq!(s.diagnostics["file:///b.rs"][0].severity, 2);
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
    fn ext_to_lsp_filetype_known_extensions() {
        assert_eq!(ext_to_lsp_filetype("rs"), Some("rust"));
        assert_eq!(ext_to_lsp_filetype("py"), Some("python"));
        assert_eq!(ext_to_lsp_filetype("pyw"), Some("python"));
        assert_eq!(ext_to_lsp_filetype("ts"), Some("typescript"));
        assert_eq!(ext_to_lsp_filetype("tsx"), Some("tsx"));
        assert_eq!(ext_to_lsp_filetype("cpp"), Some("c++"));
        assert_eq!(ext_to_lsp_filetype("cs"), Some("c#"));
        assert_eq!(ext_to_lsp_filetype("gos"), Some("gossamer"));
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
