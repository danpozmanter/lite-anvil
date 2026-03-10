use libc::{self, c_int, pid_t};
use mlua::prelude::*;
use parking_lot::Mutex;
use std::ffi::CString;

const READ_BUF_SIZE: usize = 2048;

// Mirror the C constants exactly.
const WAIT_NONE: i32 = 0;
const WAIT_DEADLINE: i32 = -1;
const WAIT_INFINITE: i32 = -2;

const STDIN_IDX: i32 = 0;
const STDOUT_IDX: i32 = 1;
const STDERR_IDX: i32 = 2;
const REDIRECT_DEFAULT: i32 = -1;
const REDIRECT_DISCARD: i32 = -2;
const REDIRECT_PARENT: i32 = -3;

const INVALID_FD: c_int = -1;

// ── Inner state ───────────────────────────────────────────────────────────────

struct ProcessInner {
    pid: pid_t,
    running: bool,
    returncode: i32,
    /// Timeout used when poll() is called with WAIT_DEADLINE (ms).
    deadline: i32,
    detached: bool,
    /// [0] = stdin write end, [1] = stdout read end, [2] = stderr read end.
    /// INVALID_FD when closed.
    fds: [c_int; 3],
}

impl ProcessInner {
    fn close_fd(&mut self, idx: usize) {
        if self.fds[idx] != INVALID_FD {
            // SAFETY: fd is valid and owned by this struct.
            unsafe { libc::close(self.fds[idx]) };
            self.fds[idx] = INVALID_FD;
        }
    }

    /// Non-blocking or timed wait. Returns true if process is still running.
    fn poll(&mut self, timeout_ms: i32) -> bool {
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
                    // WIFEXITED / WEXITSTATUS are safe bitwise ops wrapped in libc.
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
            // Sleep in 5 ms increments (matching C: SDL_Delay(timeout >= 5 ? 5 : 0)).
            if actual >= 5 {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        self.running
    }

    /// Send a signal to the whole process group, then poll once.
    fn signal(&mut self, sig: c_int) -> bool {
        // SAFETY: -self.pid targets the process group.
        let ok = unsafe { libc::kill(-self.pid, sig) == 0 };
        self.poll(WAIT_NONE);
        ok
    }
}

// ── Public UserData type ──────────────────────────────────────────────────────

pub struct ProcessHandle(Mutex<ProcessInner>);

// SAFETY: c_int / pid_t are plain integers (i32 on Linux).
// File descriptors are per-process resources safe to access from any thread.
unsafe impl Send for ProcessHandle {}
unsafe impl Sync for ProcessHandle {}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        let inner = self.0.get_mut();
        // Close FDs first to unblock any pending reads.
        inner.close_fd(0);
        inner.close_fd(1);
        inner.close_fd(2);
        if inner.running && !inner.detached {
            inner.signal(libc::SIGTERM);
            if inner.running {
                std::thread::sleep(std::time::Duration::from_millis(50));
                inner.poll(WAIT_NONE);
                if inner.running {
                    inner.signal(libc::SIGKILL);
                }
            }
        }
    }
}

// ── LuaUserData methods ───────────────────────────────────────────────────────

impl LuaUserData for ProcessHandle {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(LuaMetaMethod::ToString, |_, _, ()| Ok("Process"));

        methods.add_method("pid", |_, this, ()| Ok(this.0.lock().pid as i64));

        methods.add_method("returncode", |_, this, ()| -> LuaResult<LuaValue> {
            let mut inner = this.0.lock();
            inner.poll(WAIT_NONE);
            if inner.running {
                Ok(LuaValue::Nil)
            } else {
                Ok(LuaValue::Integer(inner.returncode as i64))
            }
        });

        methods.add_method("running", |_, this, ()| -> LuaResult<bool> {
            Ok(this.0.lock().poll(WAIT_NONE))
        });

        // wait(timeout_ms) — nil if still running, else exit code.
        methods.add_method(
            "wait",
            |_, this, timeout: Option<i32>| -> LuaResult<LuaValue> {
                let mut inner = this.0.lock();
                inner.poll(timeout.unwrap_or(0));
                if inner.running {
                    Ok(LuaValue::Nil)
                } else {
                    Ok(LuaValue::Integer(inner.returncode as i64))
                }
            },
        );

        methods.add_method(
            "read",
            |lua, this, (stream, read_size): (i32, Option<usize>)| -> LuaResult<LuaValue> {
                let n = read_size.unwrap_or(READ_BUF_SIZE);
                if stream != STDOUT_IDX && stream != STDERR_IDX {
                    return Err(LuaError::RuntimeError(
                        "error: can only read stdout(1) or stderr(2)".into(),
                    ));
                }
                g_read(lua, &mut this.0.lock(), stream as usize, n)
            },
        );

        methods.add_method(
            "read_stdout",
            |lua, this, read_size: Option<usize>| -> LuaResult<LuaValue> {
                g_read(
                    lua,
                    &mut this.0.lock(),
                    STDOUT_IDX as usize,
                    read_size.unwrap_or(READ_BUF_SIZE),
                )
            },
        );

        methods.add_method(
            "read_stderr",
            |lua, this, read_size: Option<usize>| -> LuaResult<LuaValue> {
                g_read(
                    lua,
                    &mut this.0.lock(),
                    STDERR_IDX as usize,
                    read_size.unwrap_or(READ_BUF_SIZE),
                )
            },
        );

        methods.add_method("write", |_, this, data: LuaString| -> LuaResult<LuaValue> {
            let bytes = data.as_bytes();
            let mut inner = this.0.lock();
            let fd = inner.fds[STDIN_IDX as usize];
            if fd == INVALID_FD {
                return Ok(LuaValue::Nil);
            }
            // SAFETY: fd is valid and owned; bytes slice is valid for its length.
            let ret =
                unsafe { libc::write(fd, bytes.as_ptr() as *const libc::c_void, bytes.len()) };
            if ret >= 0 {
                return Ok(LuaValue::Integer(ret as i64));
            }
            let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if err == libc::EAGAIN || err == libc::EWOULDBLOCK {
                Ok(LuaValue::Integer(0))
            } else {
                inner.signal(libc::SIGTERM);
                Err(LuaError::RuntimeError(format!(
                    "cannot write to child process: {}",
                    std::io::Error::from_raw_os_error(err)
                )))
            }
        });

        methods.add_method("close_stream", |_, this, stream: i32| -> LuaResult<bool> {
            let mut inner = this.0.lock();
            match stream {
                0..=2 => {
                    inner.close_fd(stream as usize);
                    Ok(true)
                }
                _ => Err(LuaError::RuntimeError("invalid stream index".into())),
            }
        });

        methods.add_method("terminate", |_, this, ()| -> LuaResult<bool> {
            Ok(this.0.lock().signal(libc::SIGTERM))
        });

        methods.add_method("kill", |_, this, ()| -> LuaResult<bool> {
            Ok(this.0.lock().signal(libc::SIGKILL))
        });

        methods.add_method("interrupt", |_, this, ()| -> LuaResult<bool> {
            Ok(this.0.lock().signal(libc::SIGINT))
        });
    }
}

// ── Shared read helper ────────────────────────────────────────────────────────

/// Non-blocking read from a process stdout/stderr pipe, up to `n` bytes.
fn g_read(lua: &Lua, inner: &mut ProcessInner, fd_idx: usize, n: usize) -> LuaResult<LuaValue> {
    let fd = inner.fds[fd_idx];
    if fd == INVALID_FD {
        return Ok(LuaValue::Nil);
    }
    if n == 0 {
        return Ok(LuaValue::String(lua.create_string("")?));
    }
    let mut buf = vec![0u8; n];
    let mut total = 0usize;
    let mut remaining = n;
    while remaining > 0 {
        // SAFETY: fd is valid and owned; buf is valid for `remaining` bytes at offset total.
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
            // EOF — child closed its write end.
            inner.poll(WAIT_NONE);
            break;
        } else {
            let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if err == libc::EAGAIN || err == libc::EWOULDBLOCK {
                break; // no data available right now
            } else {
                inner.signal(libc::SIGTERM);
                return Ok(LuaValue::Nil);
            }
        }
    }
    Ok(LuaValue::String(lua.create_string(&buf[..total])?))
}

// ── process.start ─────────────────────────────────────────────────────────────

fn process_start(
    lua: &Lua,
    (cmd_table, opts): (LuaTable, Option<LuaTable>),
) -> LuaResult<ProcessHandle> {
    let len = cmd_table.raw_len() as i64;
    let mut cmd_args: Vec<CString> = Vec::with_capacity(len as usize);
    for i in 1..=len {
        let s: String = cmd_table.raw_get(i)?;
        cmd_args.push(CString::new(s).map_err(|e| LuaError::RuntimeError(e.to_string()))?);
    }
    if cmd_args.is_empty() {
        return Err(LuaError::RuntimeError(
            "process.start: empty command".into(),
        ));
    }
    let argv_ptrs: Vec<*const libc::c_char> = cmd_args
        .iter()
        .map(|s| s.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();

    let mut detach = false;
    let mut deadline = 10i32; // ms
    let mut new_fds = [STDIN_IDX, STDOUT_IDX, STDERR_IDX];
    let mut cwd_cs: Option<CString> = None;
    let mut env_pairs: Vec<(CString, CString)> = Vec::new();

    if let Some(ref t) = opts {
        if let Ok(Some(v)) = t.get::<Option<bool>>("detach") {
            detach = v;
        }
        if let Ok(Some(v)) = t.get::<Option<f64>>("timeout") {
            deadline = v as i32;
        }
        if let Ok(Some(v)) = t.get::<Option<i32>>("stdin") {
            new_fds[0] = v;
        }
        if let Ok(Some(v)) = t.get::<Option<i32>>("stdout") {
            new_fds[1] = v;
        }
        if let Ok(Some(v)) = t.get::<Option<i32>>("stderr") {
            new_fds[2] = v;
        }
        for &nfd in &new_fds {
            if !(REDIRECT_PARENT..=STDERR_IDX).contains(&nfd) {
                return Err(LuaError::RuntimeError(
                    "error: redirect to handles, FILE* and paths are not supported".into(),
                ));
            }
        }
        // process.lua wraps options.env into a function returning "KEY=VALUE\0...\0\0".
        if let Ok(LuaValue::Function(env_fn)) = t.get::<LuaValue>("env") {
            let empty_t = lua.create_table()?;
            let env_str: String = env_fn.call(empty_t)?;
            env_pairs = parse_env_string(&env_str)?;
        }
        if let Ok(Some(s)) = t.get::<Option<String>>("cwd") {
            cwd_cs = Some(CString::new(s).map_err(|e| LuaError::RuntimeError(e.to_string()))?);
        }
    }

    fork_exec(argv_ptrs, detach, deadline, new_fds, env_pairs, cwd_cs)
}

/// Parse "KEY=VALUE\0KEY=VALUE\0\0" into (KEY, VALUE) CString pairs.
fn parse_env_string(s: &str) -> LuaResult<Vec<(CString, CString)>> {
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
            break; // double NUL = end of env
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

// ── Fork + exec (Unix) ────────────────────────────────────────────────────────

/// Performs fork+exec, wires up pipes, and returns a ProcessHandle on success.
fn fork_exec(
    argv_ptrs: Vec<*const libc::c_char>,
    detach: bool,
    deadline: i32,
    new_fds: [i32; 3],
    env_pairs: Vec<(CString, CString)>,
    cwd_cs: Option<CString>,
) -> LuaResult<ProcessHandle> {
    // child_pipes[stream][0=read, 1=write]
    // stdin  [0]: child reads  [0][0], parent writes [0][1]
    // stdout [1]: child writes [1][1], parent reads  [1][0]
    // stderr [2]: child writes [2][1], parent reads  [2][0]
    let mut pipes = [[INVALID_FD; 2]; 3];
    let mut ctrl = [INVALID_FD; 2]; // control pipe to detect exec failure

    macro_rules! bail {
        ($msg:expr) => {{
            close_all_pipes(&pipes, &ctrl);
            return Err(LuaError::RuntimeError($msg));
        }};
    }

    // Create 3 data pipes.
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

    // Set parent-side FDs non-blocking.
    // stdin → parent write [0][1]; stdout → parent read [1][0]; stderr → parent read [2][0].
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

    // Control pipe: write end gets FD_CLOEXEC; exec success → parent reads 0 bytes (EOF).
    if unsafe { libc::pipe(ctrl.as_mut_ptr()) } == -1 {
        bail!(format!(
            "cannot create control pipe: {}",
            std::io::Error::last_os_error()
        ));
    }
    if unsafe { libc::fcntl(ctrl[1], libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
        bail!("cannot set FD_CLOEXEC on control pipe".into());
    }

    // SAFETY: Standard Unix fork — safe to call from Rust when there are no other
    // active threads (mlua runs Lua in a single thread).
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        bail!(format!("cannot fork: {}", std::io::Error::last_os_error()));
    }

    if pid == 0 {
        // ── CHILD PROCESS ────────────────────────────────────────────────────
        // SAFETY: After fork we are in the child; only async-signal-safe or
        // exec functions are called from here.
        unsafe {
            if !detach {
                libc::setpgid(0, 0);
            }

            // Wire up stdio streams according to redirect options.
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
                // Close the parent's side of this stream's pipe.
                let parent_end = if stream == STDIN_IDX {
                    pipes[stream as usize][1]
                } else {
                    pipes[stream as usize][0]
                };
                libc::close(parent_end);
            }

            // Apply environment overrides.
            for (k, v) in &env_pairs {
                libc::setenv(k.as_ptr(), v.as_ptr(), 1);
            }

            // Change working directory.
            if let Some(ref cwd) = cwd_cs {
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

            // Become session leader if detaching.
            if detach {
                libc::setsid();
            }

            // Replace process image. FD_CLOEXEC on ctrl[1] closes it on exec success.
            libc::execvp(argv_ptrs[0], argv_ptrs.as_ptr());

            // exec failed — report errno through control pipe.
            let err = get_errno();
            let _ = libc::write(
                ctrl[1],
                &err as *const c_int as *const libc::c_void,
                std::mem::size_of::<c_int>(),
            );
            libc::_exit(-1);
        }
    }

    // ── PARENT PROCESS ────────────────────────────────────────────────────────

    // SAFETY: All FDs below are valid pipe descriptors created above.
    unsafe {
        // Close control write end; child's is either closed by exec or written on failure.
        libc::close(ctrl[1]);

        // Close child-side FDs we no longer need in the parent.
        libc::close(pipes[0][0]); // stdin read  (child reads)
        libc::close(pipes[1][1]); // stdout write (child writes)
        libc::close(pipes[2][1]); // stderr write (child writes)

        // Read from control pipe to detect exec failure.
        let mut exec_errno: c_int = 0;
        let sz = libc::read(
            ctrl[0],
            &mut exec_errno as *mut c_int as *mut libc::c_void,
            std::mem::size_of::<c_int>(),
        );
        libc::close(ctrl[0]);

        if sz > 0 {
            // exec failed — reap the child and close remaining parent FDs.
            let mut status = 0;
            libc::waitpid(pid, &mut status, 0);
            for &fd in &parent_fds {
                libc::close(fd);
            }
            return Err(LuaError::RuntimeError(format!(
                "Error creating child process: {}",
                std::io::Error::from_raw_os_error(exec_errno)
            )));
        }
    }

    // Success: hand ownership of the parent-side FDs to ProcessHandle.
    Ok(ProcessHandle(Mutex::new(ProcessInner {
        pid,
        running: true,
        returncode: 0,
        deadline,
        detached: detach,
        fds: parent_fds,
    })))
}

/// Read the current errno value. Async-signal-safe.
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

/// Close all still-valid FDs in the pipe/control arrays (error-path cleanup).
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

// ── Public module factory ─────────────────────────────────────────────────────

pub fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    let t = lua.create_table()?;

    t.set("STREAM_STDIN", STDIN_IDX)?;
    t.set("STREAM_STDOUT", STDOUT_IDX)?;
    t.set("STREAM_STDERR", STDERR_IDX)?;
    t.set("WAIT_NONE", WAIT_NONE)?;
    t.set("WAIT_DEADLINE", WAIT_DEADLINE)?;
    t.set("WAIT_INFINITE", WAIT_INFINITE)?;
    t.set("REDIRECT_DEFAULT", REDIRECT_DEFAULT)?;
    t.set("REDIRECT_STDOUT", STDOUT_IDX)?; // C: REDIRECT_STDOUT = STDOUT_FD = 1
    t.set("REDIRECT_STDERR", STDERR_IDX)?; // C: REDIRECT_STDERR = STDERR_FD = 2
    t.set("REDIRECT_PARENT", REDIRECT_PARENT)?;
    t.set("REDIRECT_DISCARD", REDIRECT_DISCARD)?;

    t.set("start", lua.create_function(process_start)?)?;

    t.set(
        "strerror",
        lua.create_function(|_, errno: i32| -> LuaResult<String> {
            Ok(std::io::Error::from_raw_os_error(errno).to_string())
        })?,
    )?;

    Ok(t)
}
