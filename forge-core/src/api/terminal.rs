use libc::{self, c_int, pid_t};
use mlua::prelude::*;
use parking_lot::Mutex;
use std::ffi::CString;

const READ_BUF_SIZE: usize = 4096;
const INVALID_FD: c_int = -1;

#[cfg(target_os = "linux")]
#[link(name = "util")]
unsafe extern "C" {}

struct TerminalInner {
    pid: pid_t,
    fd: c_int,
    running: bool,
    returncode: i32,
}

impl TerminalInner {
    fn close_fd(&mut self) {
        if self.fd != INVALID_FD {
            unsafe { libc::close(self.fd) };
            self.fd = INVALID_FD;
        }
    }

    fn poll(&mut self) -> bool {
        if !self.running {
            return false;
        }

        let mut raw_status: c_int = 0;
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

    fn signal(&mut self, sig: c_int) -> bool {
        let ok = unsafe { libc::kill(-self.pid, sig) == 0 || libc::kill(self.pid, sig) == 0 };
        self.poll();
        ok
    }
}

pub struct TerminalHandle(Mutex<TerminalInner>);

impl Drop for TerminalHandle {
    fn drop(&mut self) {
        let inner = self.0.get_mut();
        inner.close_fd();
        if inner.running {
            inner.signal(libc::SIGTERM);
            if inner.running {
                std::thread::sleep(std::time::Duration::from_millis(50));
                inner.poll();
                if inner.running {
                    inner.signal(libc::SIGKILL);
                }
            }
        }
    }
}

impl LuaUserData for TerminalHandle {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(LuaMetaMethod::ToString, |_, _, ()| Ok("Terminal"));

        methods.add_method("pid", |_, this, ()| Ok(this.0.lock().pid as i64));

        methods.add_method("running", |_, this, ()| Ok(this.0.lock().poll()));

        methods.add_method("returncode", |_, this, ()| -> LuaResult<LuaValue> {
            let mut inner = this.0.lock();
            inner.poll();
            if inner.running {
                Ok(LuaValue::Nil)
            } else {
                Ok(LuaValue::Integer(inner.returncode as i64))
            }
        });

        methods.add_method(
            "wait",
            |_, this, timeout_ms: Option<u64>| -> LuaResult<LuaValue> {
                let start = std::time::Instant::now();
                let timeout = timeout_ms.unwrap_or(0);
                let mut inner = this.0.lock();
                while inner.poll() {
                    if timeout == 0 || start.elapsed().as_millis() as u64 >= timeout {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                if inner.running {
                    Ok(LuaValue::Nil)
                } else {
                    Ok(LuaValue::Integer(inner.returncode as i64))
                }
            },
        );

        methods.add_method(
            "read",
            |lua, this, read_size: Option<usize>| -> LuaResult<LuaValue> {
                g_read(lua, &mut this.0.lock(), read_size.unwrap_or(READ_BUF_SIZE))
            },
        );

        methods.add_method("write", |_, this, data: LuaString| -> LuaResult<LuaValue> {
            let bytes = data.as_bytes();
            let mut inner = this.0.lock();
            if inner.fd == INVALID_FD {
                return Ok(LuaValue::Nil);
            }
            let ret = unsafe {
                libc::write(inner.fd, bytes.as_ptr() as *const libc::c_void, bytes.len())
            };
            if ret >= 0 {
                return Ok(LuaValue::Integer(ret as i64));
            }
            let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            if err == libc::EAGAIN || err == libc::EWOULDBLOCK {
                Ok(LuaValue::Integer(0))
            } else {
                inner.signal(libc::SIGTERM);
                Err(LuaError::RuntimeError(format!(
                    "cannot write to terminal: {}",
                    std::io::Error::from_raw_os_error(err)
                )))
            }
        });

        methods.add_method(
            "resize",
            |_, this, (cols, rows): (u16, u16)| -> LuaResult<bool> {
                let inner = this.0.lock();
                if inner.fd == INVALID_FD {
                    return Ok(false);
                }
                let winsz = libc::winsize {
                    ws_row: rows,
                    ws_col: cols,
                    ws_xpixel: 0,
                    ws_ypixel: 0,
                };
                let ok = unsafe { libc::ioctl(inner.fd, libc::TIOCSWINSZ, &winsz) == 0 };
                if ok {
                    unsafe {
                        libc::kill(inner.pid, libc::SIGWINCH);
                    }
                }
                Ok(ok)
            },
        );

        methods.add_method("terminate", |_, this, ()| {
            Ok(this.0.lock().signal(libc::SIGTERM))
        });
        methods.add_method("kill", |_, this, ()| {
            Ok(this.0.lock().signal(libc::SIGKILL))
        });
        methods.add_method("interrupt", |_, this, ()| {
            Ok(this.0.lock().signal(libc::SIGINT))
        });
        methods.add_method("close", |_, this, ()| {
            this.0.lock().close_fd();
            Ok(true)
        });
    }
}

fn g_read(lua: &Lua, inner: &mut TerminalInner, n: usize) -> LuaResult<LuaValue> {
    if inner.fd == INVALID_FD {
        return Ok(LuaValue::Nil);
    }
    if n == 0 {
        return Ok(LuaValue::String(lua.create_string("")?));
    }

    let mut buf = vec![0u8; n];
    let ret = unsafe { libc::read(inner.fd, buf.as_mut_ptr() as *mut libc::c_void, n) };
    if ret > 0 {
        return Ok(LuaValue::String(lua.create_string(&buf[..ret as usize])?));
    }
    if ret == 0 {
        inner.poll();
        return Ok(LuaValue::Nil);
    }

    let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    if err == libc::EAGAIN || err == libc::EWOULDBLOCK {
        Ok(LuaValue::String(lua.create_string("")?))
    } else {
        inner.signal(libc::SIGTERM);
        Ok(LuaValue::Nil)
    }
}

fn terminal_spawn(
    _lua: &Lua,
    (cmd_table, opts): (LuaTable, Option<LuaTable>),
) -> LuaResult<TerminalHandle> {
    let len = cmd_table.raw_len() as i64;
    let mut cmd_args: Vec<CString> = Vec::with_capacity(len as usize);
    for i in 1..=len {
        let s: String = cmd_table.raw_get(i)?;
        cmd_args.push(CString::new(s).map_err(|e| LuaError::RuntimeError(e.to_string()))?);
    }
    if cmd_args.is_empty() {
        return Err(LuaError::RuntimeError(
            "terminal.spawn: empty command".into(),
        ));
    }

    let argv_ptrs: Vec<*const libc::c_char> = cmd_args
        .iter()
        .map(|s| s.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();

    let mut cwd_cs: Option<CString> = None;
    let mut env_pairs: Vec<(CString, CString)> = Vec::new();
    let mut cols: u16 = 80;
    let mut rows: u16 = 24;

    if let Some(ref t) = opts {
        if let Ok(Some(s)) = t.get::<Option<String>>("cwd") {
            cwd_cs = Some(CString::new(s).map_err(|e| LuaError::RuntimeError(e.to_string()))?);
        }
        if let Ok(Some(v)) = t.get::<Option<u16>>("cols") {
            cols = v.max(1);
        }
        if let Ok(Some(v)) = t.get::<Option<u16>>("rows") {
            rows = v.max(1);
        }
        if let Ok(Some(env_t)) = t.get::<Option<LuaTable>>("env") {
            for pair in env_t.pairs::<String, String>() {
                let (k, v) = pair?;
                env_pairs.push((
                    CString::new(k).map_err(|e| LuaError::RuntimeError(e.to_string()))?,
                    CString::new(v).map_err(|e| LuaError::RuntimeError(e.to_string()))?,
                ));
            }
        }
    }

    let winsz = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    let mut master_fd = INVALID_FD;
    let pid = unsafe {
        libc::forkpty(
            &mut master_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::from_ref(&winsz).cast_mut(),
        )
    };
    if pid < 0 {
        return Err(LuaError::RuntimeError(format!(
            "cannot create terminal pty: {}",
            std::io::Error::last_os_error()
        )));
    }

    if pid == 0 {
        unsafe {
            libc::setpgid(0, 0);
            for (k, v) in &env_pairs {
                libc::setenv(k.as_ptr(), v.as_ptr(), 1);
            }
            if let Some(ref cwd) = cwd_cs {
                libc::chdir(cwd.as_ptr());
            }
            libc::execvp(argv_ptrs[0], argv_ptrs.as_ptr());
            libc::_exit(127);
        }
    }

    let flags = unsafe { libc::fcntl(master_fd, libc::F_GETFL, 0) };
    if flags != -1 {
        unsafe {
            libc::fcntl(master_fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }

    Ok(TerminalHandle(Mutex::new(TerminalInner {
        pid,
        fd: master_fd,
        running: true,
        returncode: 0,
    })))
}

pub fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    let t = lua.create_table()?;
    t.set("spawn", lua.create_function(terminal_spawn)?)?;
    Ok(t)
}
