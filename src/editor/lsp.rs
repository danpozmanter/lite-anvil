use crossbeam_channel::{Receiver, Sender, unbounded};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::thread;

use once_cell::sync::Lazy;
use parking_lot::Mutex;

// ── Protocol framing ─────────────────────────────────────────────────────────

/// Encode a JSON value as an LSP Content-Length framed message.
pub fn encode_message(value: &Value) -> Result<String, String> {
    let json = serde_json::to_string(value).map_err(|e| e.to_string())?;
    Ok(format!("Content-Length: {}\r\n\r\n{}", json.len(), json))
}

/// Decode LSP Content-Length framed messages from a buffer.
/// Returns (parsed_messages, remaining_buffer).
pub fn decode_messages(buffer: &str) -> Result<(Vec<Value>, String), String> {
    let mut messages = Vec::new();
    let mut remaining = buffer.to_string();
    loop {
        let Some(header_end) = remaining.find("\r\n\r\n") else {
            break;
        };
        let header = &remaining[..header_end];
        let Some(content_length) = header.lines().find_map(|line| {
            line.split_once(':').and_then(|(k, v)| {
                if k.eq_ignore_ascii_case("Content-Length") {
                    v.trim().parse::<usize>().ok()
                } else {
                    None
                }
            })
        }) else {
            return Err("invalid LSP message without Content-Length".to_string());
        };
        let body_start = header_end + 4;
        let body_end = body_start + content_length;
        if remaining.len() < body_end {
            break;
        }
        let decoded: Value =
            serde_json::from_str(&remaining[body_start..body_end]).map_err(|e| e.to_string())?;
        messages.push(decoded);
        remaining = remaining[body_end..].to_string();
    }
    Ok((messages, remaining))
}

/// LSP completion kind index to token type name.
pub fn completion_kind_name(kind: i64) -> &'static str {
    match kind {
        1 => "keyword2",
        2 | 3 => "function",
        4..=10 | 20 | 22 => "keyword2",
        11 | 21 => "literal",
        12 => "function",
        13 | 14 => "keyword",
        15 => "string",
        16 => "keyword",
        17 => "file",
        18 | 19 | 24 | 25 => "keyword",
        23 => "operator",
        _ => "keyword2",
    }
}

/// Build the completion kinds map.
pub fn completion_kinds() -> HashMap<i64, &'static str> {
    (1..=25).map(|i| (i, completion_kind_name(i))).collect()
}

// ── Transport ────────────────────────────────────────────────────────────────

/// Handle to a running LSP server process.
pub struct TransportHandle {
    pub child: Child,
    pub stdin: ChildStdin,
    pub messages: Receiver<Value>,
    pub stderr: Receiver<String>,
    pub exit_code: Arc<AtomicU64>,
}

static TRANSPORTS: Lazy<Mutex<HashMap<u64, TransportHandle>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
static NEXT_ID: Lazy<AtomicU64> = Lazy::new(|| AtomicU64::new(1));

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// Parse LSP messages from a byte buffer, sending complete messages via the channel.
pub fn parse_messages(buffer: &mut Vec<u8>, sender: &Sender<Value>) {
    loop {
        let Some(header_end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") else {
            break;
        };
        let header = String::from_utf8_lossy(&buffer[..header_end]);
        let Some(length) = header.lines().find_map(|line| {
            line.split_once(':').and_then(|(k, v)| {
                if k.eq_ignore_ascii_case("Content-Length") {
                    v.trim().parse::<usize>().ok()
                } else {
                    None
                }
            })
        }) else {
            buffer.clear();
            break;
        };
        let body_start = header_end + 4;
        let body_end = body_start + length;
        if buffer.len() < body_end {
            break;
        }
        match serde_json::from_slice::<Value>(&buffer[body_start..body_end]) {
            Ok(value) => {
                let _ = sender.send(value);
            }
            Err(e) => {
                log::warn!("LSP: malformed JSON in response, skipping message: {e}");
            }
        }
        buffer.drain(..body_end);
    }
}

fn start_stdout_thread(mut stdout: ChildStdout, sender: Sender<Value>) {
    thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    parse_messages(&mut buf, &sender);
                }
            }
        }
    });
}

fn start_stderr_thread(mut stderr: ChildStderr, sender: Sender<String>) {
    thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        loop {
            match stderr.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let text = String::from_utf8_lossy(&chunk[..n]).to_string();
                    let _ = sender.send(text);
                }
            }
        }
    });
}

/// Spawn an LSP server process and register a transport.
pub fn spawn_transport(
    command: &[String],
    cwd: &str,
    env: &[(String, String)],
) -> Result<u64, String> {
    let first = command.first().ok_or("empty LSP command")?;
    let mut cmd = Command::new(first);
    for arg in command.iter().skip(1) {
        cmd.arg(arg);
    }
    cmd.current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        cmd.env(key, value);
    }
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let stdin = child.stdin.take().ok_or("missing LSP stdin")?;
    let stdout = child.stdout.take().ok_or("missing LSP stdout")?;
    let stderr = child.stderr.take().ok_or("missing LSP stderr")?;

    let (msg_tx, msg_rx) = unbounded();
    let (err_tx, err_rx) = unbounded();
    start_stdout_thread(stdout, msg_tx);
    start_stderr_thread(stderr, err_tx);

    let id = next_id();
    TRANSPORTS.lock().insert(
        id,
        TransportHandle {
            child,
            stdin,
            messages: msg_rx,
            stderr: err_rx,
            exit_code: Arc::new(AtomicU64::new(u64::MAX)),
        },
    );
    Ok(id)
}

/// Send an LSP message (JSON value) to a transport.
pub fn send_message(id: u64, value: &Value) -> Result<(), String> {
    let payload = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    let framed = format!("Content-Length: {}\r\n\r\n", payload.len()).into_bytes();
    let mut transports = TRANSPORTS.lock();
    let handle = transports.get_mut(&id).ok_or("unknown LSP transport")?;
    handle
        .stdin
        .write_all(&framed)
        .and_then(|_| handle.stdin.write_all(&payload))
        .and_then(|_| handle.stdin.flush())
        .map_err(|e| e.to_string())
}

/// Poll result from a transport.
pub struct PollResult {
    pub messages: Vec<Value>,
    pub stderr: Vec<String>,
    pub running: bool,
    pub exit_code: Option<i64>,
}

/// Poll a transport for messages, stderr output, and process status.
pub fn poll_transport(id: u64, max_messages: usize) -> Result<PollResult, String> {
    let mut transports = TRANSPORTS.lock();
    let handle = transports.get_mut(&id).ok_or("unknown LSP transport")?;

    let mut messages = Vec::new();
    for _ in 0..max_messages {
        match handle.messages.try_recv() {
            Ok(msg) => messages.push(msg),
            Err(_) => break,
        }
    }

    let mut stderr = Vec::new();
    while let Ok(line) = handle.stderr.try_recv() {
        stderr.push(line);
    }

    let running = match handle.child.try_wait() {
        Ok(Some(status)) => {
            handle
                .exit_code
                .store(status.code().unwrap_or(-1) as u64, Ordering::Relaxed);
            false
        }
        Ok(None) => true,
        Err(_) => false,
    };

    let code = handle.exit_code.load(Ordering::Relaxed);
    let exit_code = if code != u64::MAX {
        Some(code as i64)
    } else {
        None
    };

    Ok(PollResult {
        messages,
        stderr,
        running,
        exit_code,
    })
}

/// Terminate a transport's child process.
pub fn terminate_transport(id: u64) -> bool {
    if let Some(handle) = TRANSPORTS.lock().get_mut(&id) {
        if let Err(e) = handle.child.kill() {
            log::warn!("failed to kill LSP transport {id}: {e}");
        }
        true
    } else {
        false
    }
}

/// Remove a transport.
pub fn remove_transport(id: u64) -> bool {
    TRANSPORTS.lock().remove(&id).is_some()
}

/// Terminate and remove all transports.
pub fn clear_all_transports() {
    let mut transports = TRANSPORTS.lock();
    for handle in transports.values_mut() {
        if let Err(e) = handle.child.kill() {
            log::warn!("failed to kill LSP transport: {e}");
        }
    }
    transports.clear();
    transports.shrink_to_fit();
}

// ── Manager data types ───────────────────────────────────────────────────────

/// LSP server specification.
#[derive(Clone, Debug)]
pub struct LspSpec {
    pub name: String,
    pub command: Value,
    pub filetypes: Vec<String>,
    pub root_patterns: Vec<String>,
    pub initialization_options: Option<Value>,
    pub settings: Option<Value>,
    pub env: Option<Value>,
}

/// Built-in LSP server specifications.
pub fn builtin_specs() -> Vec<LspSpec> {
    vec![
        lsp_spec(
            "rust_analyzer",
            &["rust-analyzer"],
            &["rust"],
            &["Cargo.toml", "rust-project.json", ".git"],
        ),
        lsp_spec(
            "omnisharp",
            &["OmniSharp", "-lsp"],
            &["c#"],
            &[".sln", ".csproj", ".git"],
        ),
        lsp_spec(
            "fsautocomplete",
            &["fsautocomplete", "--adaptive-lsp-server-enabled"],
            &["f#"],
            &[".fsproj", ".sln", ".git"],
        ),
        lsp_spec(
            "jdtls",
            &["jdtls"],
            &["java"],
            &["pom.xml", "build.gradle", "build.gradle.kts", ".git"],
        ),
        lsp_spec(
            "kotlin_language_server",
            &["kotlin-language-server"],
            &["kotlin"],
            &["build.gradle", "build.gradle.kts", "pom.xml", ".git"],
        ),
        lsp_spec(
            "pyright",
            &["pyright-langserver", "--stdio"],
            &["python"],
            &["pyproject.toml", "setup.py", "pyrightconfig.json", ".git"],
        ),
        lsp_spec("gopls", &["gopls"], &["go"], &["go.mod", "go.work", ".git"]),
        lsp_spec(
            "typescript_language_server",
            &["typescript-language-server", "--stdio"],
            &["javascript", "typescript", "tsx"],
            &["tsconfig.json", "jsconfig.json", "package.json", ".git"],
        ),
        lsp_spec(
            "intelephense",
            &["intelephense", "--stdio"],
            &["php"],
            &["composer.json", ".git"],
        ),
        lsp_spec(
            "elixir_ls",
            &["elixir-ls"],
            &["elixir"],
            &["mix.exs", ".git"],
        ),
        lsp_spec(
            "ocamllsp",
            &["ocamllsp"],
            &["ocaml"],
            &[".ocamlformat", "dune-project", "dune-workspace", ".git"],
        ),
        lsp_spec(
            "gleam_lsp",
            &["gleam", "lsp"],
            &["gleam"],
            &["gleam.toml", ".git"],
        ),
        lsp_spec(
            "erlang_ls",
            &["erlang_ls"],
            &["erlang"],
            &["rebar.config", "erlang.mk", ".git"],
        ),
        lsp_spec(
            "clangd",
            &["clangd"],
            &["c", "c++"],
            &[".clangd", "compile_commands.json", ".git"],
        ),
        lsp_spec(
            "haskell_language_server",
            &["haskell-language-server", "--lsp"],
            &["haskell"],
            &[
                "hie.yaml",
                "package.yaml",
                "stack.yaml",
                "cabal.project",
                ".git",
            ],
        ),
        lsp_spec(
            "lua_language_server",
            &["lua-language-server"],
            &["lua"],
            &[".luarc.json", ".luacheckrc", ".git"],
        ),
        lsp_spec(
            "svelte_language_server",
            &["svelteserver", "--stdio"],
            &["svelte"],
            &["svelte.config.js", "package.json", ".git"],
        ),
        lsp_spec("zls", &["zls"], &["zig"], &["build.zig", ".git"]),
    ]
}

fn lsp_spec(name: &str, cmd: &[&str], filetypes: &[&str], root_patterns: &[&str]) -> LspSpec {
    LspSpec {
        name: name.to_string(),
        command: Value::Array(cmd.iter().map(|s| Value::String(s.to_string())).collect()),
        filetypes: filetypes.iter().map(|s| s.to_string()).collect(),
        root_patterns: root_patterns.iter().map(|s| s.to_string()).collect(),
        initialization_options: None,
        settings: None,
        env: None,
    }
}

/// Diagnostic sort key: (line, character) from a diagnostic value.
pub fn diagnostic_start_key(value: &Value) -> (i64, i64) {
    let range = value.get("range").and_then(|r| r.get("start"));
    let line = range
        .and_then(|s| s.get("line"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let character = range
        .and_then(|s| s.get("character"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    (line, character)
}

/// Map LSP semantic token type name to editor token type.
pub fn semantic_type_name(name: &str) -> &str {
    match name {
        "namespace" | "type" | "class" | "enum" | "interface" | "struct" | "typeParameter" => {
            "keyword2"
        }
        "parameter" | "variable" | "property" | "enumMember" | "event" => "normal",
        "function" | "method" | "macro" | "decorator" => "function",
        "keyword" | "modifier" => "keyword",
        "comment" => "comment",
        "string" | "regexp" => "string",
        "number" => "number",
        "operator" => "operator",
        _ => "normal",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_round_trip() {
        let value = serde_json::json!({"method": "initialize", "id": 1});
        let encoded = encode_message(&value).unwrap();
        let (messages, remaining) = decode_messages(&encoded).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["method"], "initialize");
        assert!(remaining.is_empty());
    }

    #[test]
    fn decode_partial_message() {
        let value = serde_json::json!({"id": 1});
        let encoded = encode_message(&value).unwrap();
        let partial = &encoded[..encoded.len() - 1];
        let (messages, remaining) = decode_messages(partial).unwrap();
        assert!(messages.is_empty());
        assert!(!remaining.is_empty());
    }

    #[test]
    fn decode_multiple_messages() {
        let v1 = serde_json::json!({"id": 1});
        let v2 = serde_json::json!({"id": 2});
        let encoded = format!(
            "{}{}",
            encode_message(&v1).unwrap(),
            encode_message(&v2).unwrap()
        );
        let (messages, remaining) = decode_messages(&encoded).unwrap();
        assert_eq!(messages.len(), 2);
        assert!(remaining.is_empty());
    }

    #[test]
    fn completion_kinds_covers_range() {
        let kinds = completion_kinds();
        assert_eq!(kinds.len(), 25);
        assert_eq!(kinds[&2], "function");
        assert_eq!(kinds[&13], "keyword");
    }

    #[test]
    fn builtin_specs_not_empty() {
        let specs = builtin_specs();
        assert!(specs.len() >= 15);
        assert!(specs.iter().any(|s| s.name == "rust_analyzer"));
    }

    #[test]
    fn diagnostic_start_key_extracts_position() {
        let diag = serde_json::json!({
            "range": {"start": {"line": 5, "character": 10}}
        });
        assert_eq!(diagnostic_start_key(&diag), (5, 10));
    }

    #[test]
    fn semantic_type_name_maps_correctly() {
        assert_eq!(semantic_type_name("function"), "function");
        assert_eq!(semantic_type_name("keyword"), "keyword");
        assert_eq!(semantic_type_name("class"), "keyword2");
        assert_eq!(semantic_type_name("string"), "string");
        assert_eq!(semantic_type_name("unknown"), "normal");
    }
}
