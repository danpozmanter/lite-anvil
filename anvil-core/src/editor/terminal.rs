#[cfg(unix)]
use libc::{self, c_int, pid_t};
#[cfg(unix)]
use std::ffi::CString;

#[cfg(unix)]
const INVALID_FD: c_int = -1;

/// Terminal PTY state: master fd, child pid, size, and exit code.
#[cfg(unix)]
pub struct TerminalInner {
    pub pid: pid_t,
    pub fd: c_int,
    pub running: bool,
    pub returncode: i32,
}

#[cfg(unix)]
impl TerminalInner {
    /// Close the master PTY fd.
    pub fn close_fd(&mut self) {
        if self.fd != INVALID_FD {
            // SAFETY: fd is valid and owned by this struct.
            unsafe { libc::close(self.fd) };
            self.fd = INVALID_FD;
        }
    }

    /// Non-blocking poll. Returns true if still running.
    pub fn poll(&mut self) -> bool {
        if !self.running {
            return false;
        }
        let mut raw_status: c_int = 0;
        // SAFETY: self.pid is a valid child pid.
        let ret = unsafe { libc::waitpid(self.pid, &mut raw_status, libc::WNOHANG) };
        if ret != 0 {
            self.running = false;
            if ret > 0 {
                self.returncode = if libc::WIFEXITED(raw_status) {
                    libc::WEXITSTATUS(raw_status)
                } else {
                    -1
                };
            }
        }
        self.running
    }

    /// Send a signal to the process group and the process, then poll.
    pub fn signal(&mut self, sig: c_int) -> bool {
        // SAFETY: -self.pid targets the process group; self.pid targets the process.
        let ok = unsafe { libc::kill(-self.pid, sig) == 0 || libc::kill(self.pid, sig) == 0 };
        self.poll();
        ok
    }

    /// Non-blocking read from the PTY. Returns bytes read, empty for EAGAIN, None for closed/error.
    pub fn read(&mut self, max_bytes: usize) -> Option<Vec<u8>> {
        if self.fd == INVALID_FD {
            return None;
        }
        if max_bytes == 0 {
            return Some(Vec::new());
        }
        let mut buf = vec![0u8; max_bytes];
        // SAFETY: fd is valid and owned; buf is valid for max_bytes.
        let ret = unsafe { libc::read(self.fd, buf.as_mut_ptr() as *mut libc::c_void, max_bytes) };
        if ret > 0 {
            buf.truncate(ret as usize);
            return Some(buf);
        }
        if ret == 0 {
            self.poll();
            return None;
        }
        let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        if err == libc::EAGAIN || err == libc::EWOULDBLOCK {
            Some(Vec::new())
        } else {
            self.signal(libc::SIGTERM);
            None
        }
    }

    /// Write bytes to the PTY. Returns bytes written.
    pub fn write(&mut self, data: &[u8]) -> Result<usize, String> {
        if self.fd == INVALID_FD {
            return Ok(0);
        }
        // SAFETY: fd is valid and owned; data is valid.
        let ret = unsafe { libc::write(self.fd, data.as_ptr() as *const libc::c_void, data.len()) };
        if ret >= 0 {
            return Ok(ret as usize);
        }
        let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        if err == libc::EAGAIN || err == libc::EWOULDBLOCK {
            Ok(0)
        } else {
            self.signal(libc::SIGTERM);
            Err(format!(
                "cannot write to terminal: {}",
                std::io::Error::from_raw_os_error(err)
            ))
        }
    }

    /// Resize the PTY window.
    pub fn resize(&self, cols: u16, rows: u16) -> bool {
        if self.fd == INVALID_FD {
            return false;
        }
        let winsz = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: fd is valid; winsz is a valid winsize struct.
        let ok = unsafe { libc::ioctl(self.fd, libc::TIOCSWINSZ, &winsz) == 0 };
        if ok {
            // SAFETY: self.pid is a valid child pid.
            unsafe {
                libc::kill(self.pid, libc::SIGWINCH);
            }
        }
        ok
    }

    /// Clean up on drop: close FD, terminate if running.
    pub fn cleanup(&mut self) {
        self.close_fd();
        if self.running {
            self.signal(libc::SIGTERM);
            if self.running {
                std::thread::sleep(std::time::Duration::from_millis(50));
                self.poll();
                if self.running {
                    self.signal(libc::SIGKILL);
                }
            }
        }
    }
}

/// Options for spawning a terminal.
#[cfg(unix)]
pub struct TerminalSpawnOptions {
    pub cwd: Option<CString>,
    pub env: Vec<(CString, CString)>,
    pub cols: u16,
    pub rows: u16,
}

#[cfg(unix)]
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

/// Ensure TERM and COLORTERM are set.
#[cfg(unix)]
pub fn ensure_terminal_env(env_pairs: &mut Vec<(CString, CString)>) -> Result<(), String> {
    ensure_terminal_env_with(env_pairs, |key| std::env::var_os(key).is_some())
}

/// Testable version with injectable environment check.
#[cfg(unix)]
pub fn ensure_terminal_env_with(
    env_pairs: &mut Vec<(CString, CString)>,
    mut inherited_has: impl FnMut(&str) -> bool,
) -> Result<(), String> {
    let has_term = env_pairs.iter().any(|(key, _)| key.as_bytes() == b"TERM");
    if !has_term && !inherited_has("TERM") {
        push_env_pair(env_pairs, "TERM", "xterm-256color")?;
    }
    let has_colorterm = env_pairs
        .iter()
        .any(|(key, _)| key.as_bytes() == b"COLORTERM");
    if !has_colorterm && !inherited_has("COLORTERM") {
        push_env_pair(env_pairs, "COLORTERM", "truecolor")?;
    }
    Ok(())
}

#[cfg(unix)]
fn push_env_pair(
    env_pairs: &mut Vec<(CString, CString)>,
    key: &str,
    value: &str,
) -> Result<(), String> {
    let k = CString::new(key).map_err(|e| e.to_string())?;
    let v = CString::new(value).map_err(|e| e.to_string())?;
    env_pairs.push((k, v));
    Ok(())
}

/// Spawn a terminal subprocess via forkpty. Returns `TerminalInner` on success.
#[cfg(unix)]
pub fn spawn_terminal(
    cmd_args: &[CString],
    opts: &TerminalSpawnOptions,
) -> Result<TerminalInner, String> {
    if cmd_args.is_empty() {
        return Err("terminal.spawn: empty command".into());
    }
    let argv_ptrs: Vec<*const libc::c_char> = cmd_args
        .iter()
        .map(|s| s.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();

    let winsz = libc::winsize {
        ws_row: opts.rows,
        ws_col: opts.cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    let mut master_fd = INVALID_FD;
    // SAFETY: Standard Unix forkpty.
    let pid = unsafe {
        libc::forkpty(
            &mut master_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::from_ref(&winsz).cast_mut(),
        )
    };
    if pid < 0 {
        return Err(format!(
            "cannot create terminal pty: {}",
            std::io::Error::last_os_error()
        ));
    }

    if pid == 0 {
        // SAFETY: Child process.
        unsafe {
            libc::setpgid(0, 0);
            for (k, v) in &opts.env {
                libc::setenv(k.as_ptr(), v.as_ptr(), 1);
            }
            if let Some(ref cwd) = opts.cwd {
                libc::chdir(cwd.as_ptr());
            }
            libc::execvp(argv_ptrs[0], argv_ptrs.as_ptr());
            libc::_exit(127);
        }
    }

    // SAFETY: master_fd is a valid pty from forkpty.
    let flags = unsafe { libc::fcntl(master_fd, libc::F_GETFL, 0) };
    if flags != -1 {
        unsafe {
            libc::fcntl(master_fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }

    Ok(TerminalInner {
        pid,
        fd: master_fd,
        running: true,
        returncode: 0,
    })
}

/// A contiguous run of characters with the same fg/bg colors for rendering.
#[derive(Debug, Clone, PartialEq)]
pub struct TextRun {
    pub text: String,
    pub start_col: usize,
    pub end_col: usize,
    pub fg: Option<[u8; 4]>,
    pub bg: Option<[u8; 4]>,
}

/// Pack an RGBA color into a u32 for compact storage.
#[inline]
pub fn pack_color(color: [u8; 4]) -> u32 {
    u32::from_be_bytes(color)
}

/// Unpack a u32 into an RGBA color. Returns None for 0 (transparent/default).
#[inline]
pub fn unpack_color(color: u32) -> Option<[u8; 4]> {
    if color == 0 {
        None
    } else {
        Some(color.to_be_bytes())
    }
}

/// Convert a stored character code back to a char.
#[inline]
pub fn cell_char(ch: u32) -> char {
    char::from_u32(ch).unwrap_or(' ')
}

/// Extract text runs from a row of cells.
pub fn extract_runs(row: &[(u32, u32, u32)]) -> Vec<TextRun> {
    if row.is_empty() {
        return Vec::new();
    }
    let mut runs = Vec::new();
    let mut start = 0usize;
    while start < row.len() {
        let (_, fg, bg) = row[start];
        let mut finish = start + 1;
        let mut text = String::new();
        text.push(cell_char(row[start].0));
        while finish < row.len() && row[finish].1 == fg && row[finish].2 == bg {
            text.push(cell_char(row[finish].0));
            finish += 1;
        }
        runs.push(TextRun {
            text,
            start_col: start + 1,
            end_col: finish,
            fg: unpack_color(fg),
            bg: unpack_color(bg),
        });
        start = finish;
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_unpack_round_trip() {
        let color = [255, 128, 64, 200];
        let packed = pack_color(color);
        assert_eq!(unpack_color(packed), Some(color));
    }

    #[test]
    fn unpack_zero_is_none() {
        assert_eq!(unpack_color(0), None);
    }

    #[test]
    fn cell_char_valid() {
        assert_eq!(cell_char('A' as u32), 'A');
    }

    #[test]
    fn cell_char_invalid() {
        assert_eq!(cell_char(0xFFFFFFFF), ' ');
    }

    #[test]
    fn extract_runs_single_color() {
        let fg = pack_color([255, 255, 255, 255]);
        let row = vec![('a' as u32, fg, 0), ('b' as u32, fg, 0)];
        let runs = extract_runs(&row);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].text, "ab");
        assert_eq!(runs[0].start_col, 1);
        assert_eq!(runs[0].end_col, 2);
    }

    #[test]
    fn extract_runs_color_change() {
        let fg1 = pack_color([255, 0, 0, 255]);
        let fg2 = pack_color([0, 255, 0, 255]);
        let row = vec![('a' as u32, fg1, 0), ('b' as u32, fg2, 0)];
        let runs = extract_runs(&row);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].text, "a");
        assert_eq!(runs[1].text, "b");
    }

    #[test]
    fn extract_runs_empty() {
        let runs = extract_runs(&[]);
        assert!(runs.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn ensure_env_adds_defaults() {
        let mut env = Vec::new();
        ensure_terminal_env_with(&mut env, |_| false).unwrap();
        assert!(env.iter().any(|(k, _)| k.as_bytes() == b"TERM"));
        assert!(env.iter().any(|(k, _)| k.as_bytes() == b"COLORTERM"));
    }

    #[cfg(unix)]
    #[test]
    fn ensure_env_skips_existing() {
        use std::ffi::CString;
        let mut env = vec![(
            CString::new("TERM").unwrap(),
            CString::new("custom").unwrap(),
        )];
        ensure_terminal_env_with(&mut env, |_| false).unwrap();
        let terms: Vec<_> = env
            .iter()
            .filter(|(k, _)| k.as_bytes() == b"TERM")
            .collect();
        assert_eq!(terms.len(), 1);
        assert_eq!(terms[0].1.as_bytes(), b"custom");
    }

    #[cfg(unix)]
    fn read_until(
        term: &mut TerminalInner,
        needle: &[u8],
        timeout: std::time::Duration,
    ) -> Vec<u8> {
        use std::time::Instant;
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
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        accumulated
    }

    #[cfg(unix)]
    #[test]
    fn terminal_spawn_subprocess_output_is_readable() {
        use std::ffi::CString;
        use std::time::Duration;

        let args = [
            CString::new("/bin/sh").unwrap(),
            CString::new("-c").unwrap(),
            CString::new("printf 'hello-from-pty'").unwrap(),
        ];
        let mut term =
            spawn_terminal(&args, &TerminalSpawnOptions::default()).expect("spawn_terminal failed");

        let output = read_until(&mut term, b"hello-from-pty", Duration::from_secs(5));
        term.cleanup();

        assert!(
            output
                .windows(b"hello-from-pty".len())
                .any(|w| w == b"hello-from-pty"),
            "expected output to contain 'hello-from-pty', got: {:?}",
            String::from_utf8_lossy(&output),
        );
    }

    #[cfg(unix)]
    #[test]
    fn terminal_write_then_read_roundtrip() {
        use std::ffi::CString;
        use std::time::Duration;

        let args = [
            CString::new("/bin/sh").unwrap(),
            CString::new("-c").unwrap(),
            CString::new("read line; printf 'got:%s' \"$line\"").unwrap(),
        ];
        let mut term =
            spawn_terminal(&args, &TerminalSpawnOptions::default()).expect("spawn_terminal failed");

        term.write(b"ping\n").expect("write failed");

        let output = read_until(&mut term, b"got:ping", Duration::from_secs(5));
        term.cleanup();

        assert!(
            output.windows(b"got:ping".len()).any(|w| w == b"got:ping"),
            "expected output to contain 'got:ping', got: {:?}",
            String::from_utf8_lossy(&output),
        );
    }
}
