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

use parking_lot::Mutex;
use std::sync::LazyLock;

/// Largest LSP frame body accepted. Bounds buffer growth and rejects a
/// corrupt or hostile Content-Length before it is used as a slice bound.
const MAX_LSP_FRAME: usize = 64 << 20;

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
    let mut pos = 0usize;
    loop {
        let slice = &buffer[pos..];
        let Some(header_end) = slice.find("\r\n\r\n") else {
            break;
        };
        let header = &slice[..header_end];
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
        if content_length > MAX_LSP_FRAME {
            return Err("LSP Content-Length exceeds frame cap".to_string());
        }
        let body_start = pos + header_end + 4;
        let Some(body_end) = body_start.checked_add(content_length) else {
            return Err("LSP Content-Length overflow".to_string());
        };
        if buffer.len() < body_end {
            break;
        }
        let decoded: Value =
            serde_json::from_str(&buffer[body_start..body_end]).map_err(|e| e.to_string())?;
        messages.push(decoded);
        pos = body_end;
    }
    Ok((messages, buffer[pos..].to_string()))
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
    /// Framed messages bound for the server's stdin. The dedicated writer
    /// thread owns the pipe and drains this channel, so senders never block
    /// the UI thread on a full pipe buffer. Dropping the handle closes the
    /// channel, which stops the writer thread.
    pub writer: Sender<Vec<u8>>,
    pub messages: Receiver<Value>,
    pub stderr: Receiver<String>,
    pub exit_code: Arc<AtomicU64>,
}

impl Drop for TransportHandle {
    /// Kill and reap the server on every drop path, including a panic-unwind.
    /// `Child::drop` detaches without reaping, so without this a dropped handle
    /// leaks the language server. Best-effort and idempotent: an already-exited
    /// child (or a prior explicit kill/wait) makes both calls harmless no-ops,
    /// since `Child::wait` returns the cached status without a second reap.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

static TRANSPORTS: LazyLock<Mutex<HashMap<u64, TransportHandle>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT_ID: LazyLock<AtomicU64> = LazyLock::new(|| AtomicU64::new(1));

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// Parse LSP messages from a byte buffer, sending complete messages via the channel.
pub fn parse_messages(buffer: &mut Vec<u8>, sender: &Sender<Value>) {
    while let Some(header_end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
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
        if length > MAX_LSP_FRAME {
            buffer.clear();
            break;
        }
        let body_start = header_end + 4;
        let Some(body_end) = body_start.checked_add(length) else {
            buffer.clear();
            break;
        };
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
                    // Wake the (possibly idle-blocked) render loop so it drains
                    // the freshly parsed messages instead of waiting out its
                    // idle timeout.
                    crate::window::push_wakeup_event();
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

fn start_writer_thread(mut stdin: ChildStdin, receiver: Receiver<Vec<u8>>) {
    thread::spawn(move || {
        while let Ok(frame) = receiver.recv() {
            if stdin.write_all(&frame).and_then(|_| stdin.flush()).is_err() {
                break;
            }
        }
    });
}

/// Set up the spawned LSP server so it does not outlive this editor process.
#[cfg(target_os = "linux")]
fn configure_child_lifetime(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: pre_exec runs in the forked child before exec; prctl is
    // async-signal-safe. PR_SET_PDEATHSIG delivers SIGTERM to this child
    // when the editor process dies, so an orphaned server is reaped.
    unsafe {
        cmd.pre_exec(|| {
            if libc::prctl(
                libc::PR_SET_PDEATHSIG,
                libc::SIGTERM as libc::c_ulong,
                0,
                0,
                0,
            ) == -1
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Set up the spawned LSP server so it can be killed as a group.
#[cfg(target_os = "macos")]
fn configure_child_lifetime(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: pre_exec runs in the forked child before exec; setsid is
    // async-signal-safe. A fresh session/process-group lets the editor
    // group-kill the server instead of leaving it orphaned (macOS has no
    // PR_SET_PDEATHSIG equivalent).
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn configure_child_lifetime(_cmd: &mut Command) {}

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
    configure_child_lifetime(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    let stdin = child.stdin.take().ok_or("missing LSP stdin")?;
    let stdout = child.stdout.take().ok_or("missing LSP stdout")?;
    let stderr = child.stderr.take().ok_or("missing LSP stderr")?;

    let (msg_tx, msg_rx) = unbounded();
    let (err_tx, err_rx) = unbounded();
    let (writer_tx, writer_rx) = unbounded();
    start_stdout_thread(stdout, msg_tx);
    start_stderr_thread(stderr, err_tx);
    start_writer_thread(stdin, writer_rx);

    let id = next_id();
    TRANSPORTS.lock().insert(
        id,
        TransportHandle {
            child,
            writer: writer_tx,
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
    let mut frame = format!("Content-Length: {}\r\n\r\n", payload.len()).into_bytes();
    frame.extend_from_slice(&payload);
    let writer = {
        let transports = TRANSPORTS.lock();
        transports
            .get(&id)
            .ok_or("unknown LSP transport")?
            .writer
            .clone()
    };
    writer.send(frame).map_err(|e| e.to_string())
}

/// Poll result from a transport.
pub struct PollResult {
    pub messages: Vec<Value>,
    pub stderr: Vec<String>,
    pub running: bool,
    pub exit_code: Option<i64>,
}

/// Poll a transport for messages, stderr output, and process status.
///
/// Drains all currently queued messages: capping the per-frame count let the
/// unbounded channel accumulate a backlog (and unbounded memory) whenever a
/// server emitted more than the cap in one frame, e.g. a burst of streamed
/// diagnostics or semantic tokens on a large workspace.
pub fn poll_transport(id: u64) -> Result<PollResult, String> {
    let mut transports = TRANSPORTS.lock();
    let handle = transports.get_mut(&id).ok_or("unknown LSP transport")?;

    let mut messages = Vec::new();
    while let Ok(msg) = handle.messages.try_recv() {
        messages.push(msg);
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

/// Terminate a transport's child process, reaping it so no zombie remains.
pub fn terminate_transport(id: u64) -> bool {
    if let Some(handle) = TRANSPORTS.lock().get_mut(&id) {
        if let Err(e) = handle.child.kill() {
            log::warn!("failed to kill LSP transport {id}: {e}");
        }
        let _ = handle.child.wait();
        true
    } else {
        false
    }
}

/// Remove a transport, closing its writer channel and reaping the child.
pub fn remove_transport(id: u64) -> bool {
    let handle = TRANSPORTS.lock().remove(&id);
    if let Some(mut handle) = handle {
        // The child may already have exited; kill is best-effort.
        let _ = handle.child.kill();
        let _ = handle.child.wait();
        // Dropping `handle` closes the writer Sender, stopping the writer thread.
        true
    } else {
        false
    }
}

/// Terminate every transport, reaping each child and closing its writer thread.
pub fn clear_all_transports() {
    let handles: Vec<TransportHandle> = {
        let mut transports = TRANSPORTS.lock();
        transports.drain().map(|(_, handle)| handle).collect()
    };
    for mut handle in handles {
        if let Err(e) = handle.child.kill() {
            log::warn!("failed to kill LSP transport: {e}");
        }
        let _ = handle.child.wait();
        // Each `handle` drops here, closing its writer Sender so the thread exits.
    }
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
        lsp_spec(
            "gossamer_lsp",
            &["gos", "lsp"],
            &["gossamer"],
            &["project.toml", ".git"],
        ),
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
    fn decode_messages_rejects_oversized_content_length() {
        let buffer = "Content-Length: 999999999\r\n\r\n{}";
        assert!(decode_messages(buffer).is_err());
    }

    #[test]
    fn parse_messages_clears_buffer_on_oversized_content_length() {
        let (tx, rx) = unbounded();
        let mut buffer = b"Content-Length: 999999999\r\n\r\n{}".to_vec();
        parse_messages(&mut buffer, &tx);
        assert!(buffer.is_empty());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn parse_messages_handles_max_content_length_without_panic() {
        let (tx, rx) = unbounded();
        let header = format!("Content-Length: {}\r\n\r\n", usize::MAX);
        let mut buffer = header.into_bytes();
        parse_messages(&mut buffer, &tx);
        assert!(buffer.is_empty());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn parse_messages_decodes_valid_frame() {
        let (tx, rx) = unbounded();
        let value = serde_json::json!({"id": 1});
        let mut buffer = encode_message(&value).unwrap().into_bytes();
        parse_messages(&mut buffer, &tx);
        assert_eq!(rx.try_recv().unwrap()["id"], 1);
        assert!(buffer.is_empty());
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
