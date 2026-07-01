#[cfg(windows)]
use std::io::{Read, Write};
#[cfg(windows)]
use std::process::{Child, ChildStdin, Command, Stdio};

#[cfg(windows)]
use crossbeam_channel::{Receiver, Sender};

/// Bytes drained from a child pipe per read; also the upper bound on one buffered chunk.
#[cfg(windows)]
const READER_CHUNK: usize = 4096;

/// Buffered reader chunks before the reader thread blocks on `send`, capping the
/// backlog near 8 MiB. A Windows pipe has no flow control, so this bound is what
/// restores backpressure: once it fills, the reader stalls its pipe read and
/// throttles a flooding child, mirroring Unix PTY semantics.
#[cfg(windows)]
const READER_BACKLOG: usize = (8 * 1024 * 1024) / READER_CHUNK;

/// Terminal process inner state for Windows, using piped stdin/stdout.
#[cfg(windows)]
pub struct TerminalInner {
    child: Child,
    stdin_pipe: ChildStdin,
    reader_rx: Receiver<Vec<u8>>,
    _stdout_thread: Option<std::thread::JoinHandle<()>>,
    _stderr_thread: Option<std::thread::JoinHandle<()>>,
    pub running: bool,
    pub returncode: i32,
}

/// Options for spawning a terminal on Windows.
#[cfg(windows)]
pub struct TerminalSpawnOptions {
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
}

#[cfg(windows)]
impl Default for TerminalSpawnOptions {
    fn default() -> Self {
        Self {
            cwd: None,
            env: Vec::new(),
            cols: 80,
            rows: 24,
        }
    }
}

#[cfg(windows)]
impl TerminalInner {
    /// Non-blocking poll. Returns true if still running.
    pub fn poll(&mut self) -> bool {
        if !self.running {
            return false;
        }
        match self.child.try_wait() {
            Ok(Some(status)) => {
                self.running = false;
                self.returncode = status.code().unwrap_or(-1);
            }
            Ok(None) => {}
            Err(_) => {
                self.running = false;
                self.returncode = -1;
            }
        }
        self.running
    }

    /// Non-blocking read from the child's stdout via channel. Returns bytes read,
    /// empty vec for no data available, None for closed/error.
    pub fn read(&mut self, max_bytes: usize) -> Option<Vec<u8>> {
        if !self.running && self.reader_rx.is_empty() {
            return None;
        }
        if max_bytes == 0 {
            return Some(Vec::new());
        }
        let mut collected = Vec::new();
        while collected.len() < max_bytes {
            match self.reader_rx.try_recv() {
                Ok(chunk) => collected.extend_from_slice(&chunk),
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    if collected.is_empty() {
                        self.poll();
                        return None;
                    }
                    break;
                }
            }
        }
        if collected.len() > max_bytes {
            collected.truncate(max_bytes);
        }
        Some(collected)
    }

    /// Write bytes to the child's stdin. Returns bytes written.
    pub fn write(&mut self, data: &[u8]) -> Result<usize, String> {
        if !self.running {
            return Ok(0);
        }
        self.stdin_pipe
            .write(data)
            .map_err(|e| format!("cannot write to terminal: {e}"))
    }

    /// Resize is a no-op for piped I/O (no PTY to resize).
    pub fn resize(&self, _cols: u16, _rows: u16) -> bool {
        false
    }

    /// Clean up: kill the child process.
    pub fn cleanup(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.running = false;
    }
}

/// Ensure TERM and COLORTERM are set so child processes emit ANSI codes.
#[cfg(windows)]
pub fn ensure_terminal_env(env_pairs: &mut Vec<(String, String)>) -> Result<(), String> {
    let has_term = env_pairs.iter().any(|(k, _)| k == "TERM");
    if !has_term && std::env::var_os("TERM").is_none() {
        env_pairs.push(("TERM".to_string(), "xterm-256color".to_string()));
    }
    let has_colorterm = env_pairs.iter().any(|(k, _)| k == "COLORTERM");
    if !has_colorterm && std::env::var_os("COLORTERM").is_none() {
        env_pairs.push(("COLORTERM".to_string(), "truecolor".to_string()));
    }
    Ok(())
}

/// Spawn a terminal subprocess on Windows with piped I/O and a reader thread.
#[cfg(windows)]
pub fn spawn_terminal(opts: &TerminalSpawnOptions) -> Result<TerminalInner, String> {
    let shell = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());

    let mut cmd = Command::new(&shell);
    // /Q suppresses command echo, /K keeps the shell running.
    if shell.to_lowercase().contains("cmd") {
        cmd.arg("/Q");
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if let Some(ref cwd) = opts.cwd {
        cmd.current_dir(cwd);
    }

    for (k, v) in &opts.env {
        cmd.env(k, v);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot spawn terminal: {e}"))?;

    let stdin_pipe = child
        .stdin
        .take()
        .ok_or_else(|| "failed to capture child stdin".to_string())?;
    let stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| "failed to capture child stdout".to_string())?;
    let stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| "failed to capture child stderr".to_string())?;

    let (tx, rx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = crossbeam_channel::bounded(READER_BACKLOG);

    let stdout_tx = tx.clone();
    let stdout_handle = std::thread::Builder::new()
        .name("terminal-stdout-reader".to_string())
        .spawn(move || reader_thread(stdout_pipe, stdout_tx))
        .map_err(|e| format!("cannot spawn stdout reader thread: {e}"))?;

    let stderr_tx = tx;
    let stderr_handle = std::thread::Builder::new()
        .name("terminal-stderr-reader".to_string())
        .spawn(move || reader_thread(stderr_pipe, stderr_tx))
        .map_err(|e| format!("cannot spawn stderr reader thread: {e}"))?;

    Ok(TerminalInner {
        child,
        stdin_pipe,
        reader_rx: rx,
        _stdout_thread: Some(stdout_handle),
        _stderr_thread: Some(stderr_handle),
        running: true,
        returncode: 0,
    })
}

/// Blocking read loop that sends chunks through a channel until EOF.
#[cfg(windows)]
fn reader_thread(mut stream: impl Read + Send + 'static, tx: Sender<Vec<u8>>) {
    let mut buf = [0u8; READER_CHUNK];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

#[cfg(windows)]
#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn read_until(term: &mut TerminalInner, needle: &[u8], timeout: Duration) -> Vec<u8> {
        let deadline = Instant::now() + timeout;
        let mut accumulated = Vec::new();
        while Instant::now() < deadline {
            term.poll();
            if let Some(bytes) = term.read(4096) {
                if !bytes.is_empty() {
                    accumulated.extend_from_slice(&bytes);
                    if accumulated.windows(needle.len()).any(|w| w == needle) {
                        return accumulated;
                    }
                    continue;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        accumulated
    }

    #[test]
    fn terminal_spawn_cmd_echo_roundtrip() {
        let mut term =
            spawn_terminal(&TerminalSpawnOptions::default()).expect("spawn_terminal failed");

        term.write(b"echo hello-from-cmd\r\nexit 0\r\n")
            .expect("write failed");

        let output = read_until(&mut term, b"hello-from-cmd", Duration::from_secs(10));
        term.cleanup();

        let s = String::from_utf8_lossy(&output);
        assert!(
            s.contains("hello-from-cmd"),
            "expected output to contain 'hello-from-cmd', got: {s:?}",
        );
    }
}
