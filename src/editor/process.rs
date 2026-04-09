use libc::{self, c_int, pid_t};
use std::ffi::CString;

pub const READ_BUF_SIZE: usize = 2048;

pub const WAIT_NONE: i32 = 0;
pub const WAIT_DEADLINE: i32 = -1;
pub const WAIT_INFINITE: i32 = -2;

pub const STDIN_IDX: i32 = 0;
pub const STDOUT_IDX: i32 = 1;
pub const STDERR_IDX: i32 = 2;
pub const REDIRECT_DEFAULT: i32 = -1;
pub const REDIRECT_DISCARD: i32 = -2;
pub const REDIRECT_PARENT: i32 = -3;

pub const INVALID_FD: c_int = -1;

/// Process operation errors.
#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("{0}")]
    Runtime(String),
}

/// Core subprocess state, independent of Lua.
pub struct ProcessInner {
    pub pid: pid_t,
    pub running: bool,
    pub returncode: i32,
    pub deadline: i32,
    pub detached: bool,
    pub fds: [c_int; 3],
}

impl ProcessInner {
    /// Close a file descriptor by index.
    pub fn close_fd(&mut self, idx: usize) {
        if self.fds[idx] != INVALID_FD {
            // SAFETY: fd is valid and owned by this struct.
            unsafe { libc::close(self.fds[idx]) };
            self.fds[idx] = INVALID_FD;
        }
    }

    /// Non-blocking or timed wait. Returns true if process is still running.
    pub fn poll(&mut self, timeout_ms: i32) -> bool {
        if !self.running {
            return false;
        }
        let actual = if timeout_ms == WAIT_DEADLINE {
            self.deadline
        } else {
            timeout_ms
        };

        let start = std::time::Instant::now();
        loop {
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
                break;
            }
            if actual == WAIT_NONE {
                break;
            }
            let elapsed_ms = start.elapsed().as_millis() as i32;
            if actual != WAIT_INFINITE && elapsed_ms >= actual {
                break;
            }
            if actual >= 5 {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        self.running
    }

    /// Send a signal to the process group, then poll once.
    pub fn signal(&mut self, sig: c_int) -> bool {
        // SAFETY: -self.pid targets the process group.
        let ok = unsafe { libc::kill(-self.pid, sig) == 0 };
        self.poll(WAIT_NONE);
        ok
    }

    /// Non-blocking read from stdout/stderr. Returns bytes read, or None on closed/error.
    pub fn read(&mut self, fd_idx: usize, max_bytes: usize) -> Option<Vec<u8>> {
        let fd = self.fds[fd_idx];
        if fd == INVALID_FD {
            return None;
        }
        if max_bytes == 0 {
            return Some(Vec::new());
        }
        let mut buf = vec![0u8; max_bytes];
        let mut total = 0usize;
        let mut remaining = max_bytes;
        while remaining > 0 {
            // SAFETY: fd is valid; buf is valid for `remaining` bytes at offset total.
            let ret = unsafe {
                libc::read(
                    fd,
                    buf.as_mut_ptr().add(total) as *mut libc::c_void,
                    remaining,
                )
            };
            if ret > 0 {
                total += ret as usize;
                remaining -= ret as usize;
            } else if ret == 0 {
                self.close_fd(fd_idx);
                self.poll(WAIT_NONE);
                break;
            } else {
                let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
                if err == libc::EAGAIN || err == libc::EWOULDBLOCK {
                    break;
                } else {
                    self.signal(libc::SIGTERM);
                    return None;
                }
            }
        }
        buf.truncate(total);
        Some(buf)
    }

    /// Write bytes to stdin. Returns bytes written, or None on closed/error.
    pub fn write(&mut self, data: &[u8]) -> Result<usize, ProcessError> {
        let fd = self.fds[STDIN_IDX as usize];
        if fd == INVALID_FD {
            return Ok(0);
        }
        // SAFETY: fd is valid and owned; data slice is valid.
        let ret = unsafe { libc::write(fd, data.as_ptr() as *const libc::c_void, data.len()) };
        if ret >= 0 {
            return Ok(ret as usize);
        }
        let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        if err == libc::EAGAIN || err == libc::EWOULDBLOCK {
            Ok(0)
        } else {
            self.signal(libc::SIGTERM);
            Err(ProcessError::Runtime(format!(
                "cannot write to child process: {}",
                std::io::Error::from_raw_os_error(err)
            )))
        }
    }

    /// Clean up on drop: close FDs, terminate if running.
    pub fn cleanup(&mut self) {
        self.close_fd(0);
        self.close_fd(1);
        self.close_fd(2);
        if self.running && !self.detached {
            self.signal(libc::SIGTERM);
            if self.running {
                std::thread::sleep(std::time::Duration::from_millis(50));
                self.poll(WAIT_NONE);
                if self.running {
                    self.signal(libc::SIGKILL);
                }
            }
        }
    }
}

/// Options for spawning a subprocess.
pub struct SpawnOptions {
    pub detach: bool,
    pub deadline: i32,
    pub stdin_redirect: i32,
    pub stdout_redirect: i32,
    pub stderr_redirect: i32,
    pub cwd: Option<CString>,
    pub env: Vec<(CString, CString)>,
}

impl Default for SpawnOptions {
    fn default() -> Self {
        Self {
            detach: false,
            deadline: 10,
            stdin_redirect: STDIN_IDX,
            stdout_redirect: STDOUT_IDX,
            stderr_redirect: STDERR_IDX,
            cwd: None,
            env: Vec::new(),
        }
    }
}

/// Parse "KEY=VALUE\0KEY=VALUE\0\0" into (KEY, VALUE) CString pairs.
pub fn parse_env_string(s: &str) -> Result<Vec<(CString, CString)>, ProcessError> {
    let bytes = s.as_bytes();
    let mut pairs = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let end = bytes[i..]
            .iter()
            .position(|&b| b == 0)
            .map(|p| i + p)
            .unwrap_or(bytes.len());
        let entry = &bytes[i..end];
        if entry.is_empty() {
            break;
        }
        if let Some(eq) = entry.iter().position(|&b| b == b'=') {
            if let (Ok(k), Ok(v)) = (CString::new(&entry[..eq]), CString::new(&entry[eq + 1..])) {
                pairs.push((k, v));
            }
        }
        i = end + 1;
    }
    Ok(pairs)
}

/// Spawn a subprocess via fork+exec. Returns `ProcessInner` on success.
pub fn spawn(cmd_args: &[CString], opts: &SpawnOptions) -> Result<ProcessInner, ProcessError> {
    if cmd_args.is_empty() {
        return Err(ProcessError::Runtime("process.start: empty command".into()));
    }
    let argv_ptrs: Vec<*const libc::c_char> = cmd_args
        .iter()
        .map(|s| s.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();

    let new_fds = [
        opts.stdin_redirect,
        opts.stdout_redirect,
        opts.stderr_redirect,
    ];
    for &nfd in &new_fds {
        if !(REDIRECT_PARENT..=STDERR_IDX).contains(&nfd) {
            return Err(ProcessError::Runtime(
                "error: redirect to handles, FILE* and paths are not supported".into(),
            ));
        }
    }

    fork_exec(
        argv_ptrs,
        opts.detach,
        opts.deadline,
        new_fds,
        &opts.env,
        opts.cwd.as_ref(),
    )
}

fn fork_exec(
    argv_ptrs: Vec<*const libc::c_char>,
    detach: bool,
    deadline: i32,
    new_fds: [i32; 3],
    env_pairs: &[(CString, CString)],
    cwd_cs: Option<&CString>,
) -> Result<ProcessInner, ProcessError> {
    let mut pipes = [[INVALID_FD; 2]; 3];
    let mut ctrl = [INVALID_FD; 2];

    macro_rules! bail {
        ($msg:expr) => {{
            close_all_pipes(&pipes, &ctrl);
            return Err(ProcessError::Runtime($msg));
        }};
    }

    for i in 0..3 {
        let mut fds = [INVALID_FD; 2];
        // SAFETY: fds is a valid 2-element array.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } == -1 {
            bail!(format!(
                "cannot create pipe: {}",
                std::io::Error::last_os_error()
            ));
        }
        pipes[i] = fds;
    }

    let parent_fds = [pipes[0][1], pipes[1][0], pipes[2][0]];
    for &fd in &parent_fds {
        // SAFETY: fd is a valid pipe file descriptor.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
        if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1
        {
            bail!(format!(
                "cannot set O_NONBLOCK: {}",
                std::io::Error::last_os_error()
            ));
        }
    }

    if unsafe { libc::pipe(ctrl.as_mut_ptr()) } == -1 {
        bail!(format!(
            "cannot create control pipe: {}",
            std::io::Error::last_os_error()
        ));
    }
    if unsafe { libc::fcntl(ctrl[1], libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
        bail!("cannot set FD_CLOEXEC on control pipe".into());
    }

    // SAFETY: Standard Unix fork.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        bail!(format!("cannot fork: {}", std::io::Error::last_os_error()));
    }

    if pid == 0 {
        // SAFETY: Child process, only async-signal-safe functions called.
        unsafe {
            if !detach {
                libc::setpgid(0, 0);
            }
            for stream in 0..3i32 {
                let nfd = new_fds[stream as usize];
                if nfd == REDIRECT_DISCARD {
                    let child_end = if stream == STDIN_IDX {
                        pipes[stream as usize][0]
                    } else {
                        pipes[stream as usize][1]
                    };
                    libc::close(child_end);
                    libc::close(stream);
                } else if nfd != REDIRECT_PARENT {
                    let src_end = if nfd == STDIN_IDX {
                        pipes[nfd as usize][0]
                    } else {
                        pipes[nfd as usize][1]
                    };
                    libc::dup2(src_end, stream);
                }
                let parent_end = if stream == STDIN_IDX {
                    pipes[stream as usize][1]
                } else {
                    pipes[stream as usize][0]
                };
                libc::close(parent_end);
            }
            for (k, v) in env_pairs {
                libc::setenv(k.as_ptr(), v.as_ptr(), 1);
            }
            if let Some(cwd) = cwd_cs {
                if libc::chdir(cwd.as_ptr()) == -1 {
                    let err = get_errno();
                    let _ = libc::write(
                        ctrl[1],
                        &err as *const c_int as *const libc::c_void,
                        std::mem::size_of::<c_int>(),
                    );
                    libc::_exit(-1);
                }
            }
            if detach {
                libc::setsid();
            }
            libc::execvp(argv_ptrs[0], argv_ptrs.as_ptr());
            let err = get_errno();
            let _ = libc::write(
                ctrl[1],
                &err as *const c_int as *const libc::c_void,
                std::mem::size_of::<c_int>(),
            );
            libc::_exit(-1);
        }
    }

    // SAFETY: Parent process, all FDs are valid pipe descriptors.
    unsafe {
        libc::close(ctrl[1]);
        libc::close(pipes[0][0]);
        libc::close(pipes[1][1]);
        libc::close(pipes[2][1]);

        let mut exec_errno: c_int = 0;
        let sz = libc::read(
            ctrl[0],
            &mut exec_errno as *mut c_int as *mut libc::c_void,
            std::mem::size_of::<c_int>(),
        );
        libc::close(ctrl[0]);

        if sz > 0 {
            let mut status = 0;
            libc::waitpid(pid, &mut status, 0);
            for &fd in &parent_fds {
                libc::close(fd);
            }
            return Err(ProcessError::Runtime(format!(
                "Error creating child process: {}",
                std::io::Error::from_raw_os_error(exec_errno)
            )));
        }
    }

    Ok(ProcessInner {
        pid,
        running: true,
        returncode: 0,
        deadline,
        detached: detach,
        fds: parent_fds,
    })
}

fn get_errno() -> c_int {
    #[cfg(target_os = "linux")]
    unsafe {
        *libc::__errno_location()
    }
    #[cfg(not(target_os = "linux"))]
    unsafe {
        *libc::__error()
    }
}

fn close_all_pipes(pipes: &[[c_int; 2]; 3], ctrl: &[c_int; 2]) {
    for p in pipes {
        for &fd in p {
            if fd != INVALID_FD {
                // SAFETY: fd is valid.
                unsafe { libc::close(fd) };
            }
        }
    }
    for &fd in ctrl {
        if fd != INVALID_FD {
            unsafe { libc::close(fd) };
        }
    }
}

/// Error code to string.
pub fn strerror(errno: i32) -> String {
    std::io::Error::from_raw_os_error(errno).to_string()
}
