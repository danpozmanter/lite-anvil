mod dirmonitor;
mod doc_layout;
mod doc_native;
mod git_native;
mod lsp_manager;
mod lsp_transport;
mod markdown;
mod node_model;
mod picker;
#[cfg(unix)]
mod process;
mod project_fs;
mod project_manifest;
mod project_model;
mod project_search;
mod regex;
mod status_model;
mod symbol_index;
#[cfg(unix)]
mod terminal;
mod terminal_buffer;
mod tree_model;
mod tokenizer;
mod utf8extra;

use mlua::prelude::*;
use parking_lot::Mutex;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

// ── Fuzzy match ───────────────────────────────────────────────────────────────

/// Port of the C `f_fuzzy_match` algorithm.
/// Returns `None` if needle is not a subsequence of haystack.
/// Returns `Some(score)` otherwise (higher = better match).
/// When `files=true`, matches backwards for better filename relevance.
fn fuzzy_match(haystack: &str, needle: &str, files: bool) -> Option<i64> {
    let hb = haystack.as_bytes();
    let nb = needle.as_bytes();
    let h_len = hb.len();
    let n_len = nb.len();
    if n_len == 0 {
        return Some(-(h_len as i64) * 10);
    }
    let mut score: i64 = 0;
    let mut run: i64 = 0;
    let mut hi: isize = if files { h_len as isize - 1 } else { 0 };
    let mut ni: isize = if files { n_len as isize - 1 } else { 0 };
    let step: isize = if files { -1 } else { 1 };
    let in_h = |i: isize| i >= 0 && i < h_len as isize;
    let in_n = |i: isize| i >= 0 && i < n_len as isize;
    while in_h(hi) && in_n(ni) {
        while in_h(hi) && hb[hi as usize] == b' ' {
            hi += step;
        }
        while in_n(ni) && nb[ni as usize] == b' ' {
            ni += step;
        }
        if !in_h(hi) || !in_n(ni) {
            break;
        }
        let hc = hb[hi as usize];
        let nc = nb[ni as usize];
        if hc.eq_ignore_ascii_case(&nc) {
            score += run * 10 - if hc != nc { 1 } else { 0 };
            run += 1;
            ni += step;
        } else {
            score -= 10;
            run = 0;
        }
        hi += step;
    }
    if in_n(ni) {
        return None;
    }
    Some(score - h_len as i64 * 10)
}

// ── Path compare ──────────────────────────────────────────────────────────────

/// Port of the C `f_path_compare` natural-sort comparison.
/// Returns `true` if path1 should sort before path2.
/// Directories sort before files; numeric segments use natural ordering.
fn path_compare(path1: &str, type1: &str, path2: &str, type2: &str) -> bool {
    const SEP: u8 = b'/';
    let p1 = path1.as_bytes();
    let p2 = path2.as_bytes();
    let len1 = p1.len();
    let len2 = p2.len();
    let mut t1: i32 = if type1 != "dir" { 1 } else { 0 };
    let mut t2: i32 = if type2 != "dir" { 1 } else { 0 };
    // Common prefix: track last separator position.
    let mut offset = 0usize;
    for k in 0..len1.min(len2) {
        if p1[k] != p2[k] {
            break;
        }
        if p1[k] == SEP {
            offset = k + 1;
        }
    }
    if p1[offset..].contains(&SEP) {
        t1 = 0;
    }
    if p2[offset..].contains(&SEP) {
        t2 = 0;
    }
    if t1 != t2 {
        return t1 < t2;
    }
    let same_len = len1 == len2;
    let mut cfr: i32 = -1;
    let mut i = offset;
    let mut j = offset;
    loop {
        if i > len1 || j > len2 {
            break;
        }
        let a = if i < len1 { p1[i] } else { 0u8 };
        let b = if j < len2 { p2[j] } else { 0u8 };
        if a == 0 || b == 0 {
            if cfr < 0 {
                cfr = 0;
            }
            if !same_len {
                cfr = if a == 0 { 1 } else { 0 };
            }
            break;
        }
        if a.is_ascii_digit() && b.is_ascii_digit() {
            let mut ii = 0;
            while i + ii < len1 && p1[i + ii].is_ascii_digit() {
                ii += 1;
            }
            let mut ij = 0;
            while j + ij < len2 && p2[j + ij].is_ascii_digit() {
                ij += 1;
            }
            let mut di: u64 = 0;
            for k in 0..ii {
                di = di
                    .saturating_mul(10)
                    .saturating_add((p1[i + k] - b'0') as u64);
            }
            let mut dj: u64 = 0;
            for k in 0..ij {
                dj = dj
                    .saturating_mul(10)
                    .saturating_add((p2[j + k] - b'0') as u64);
            }
            if di != dj {
                cfr = if di < dj { 1 } else { 0 };
                break;
            }
            i += 1;
            j += 1;
            continue;
        }
        if a == b {
            i += 1;
            j += 1;
            continue;
        }
        if a == SEP || b == SEP {
            cfr = if a == SEP { 1 } else { 0 };
            break;
        }
        let al = a.to_ascii_lowercase();
        let bl = b.to_ascii_lowercase();
        if al == bl {
            if same_len && cfr < 0 {
                cfr = if a > b { 1 } else { 0 };
            }
            i += 1;
            j += 1;
            continue;
        }
        cfr = if al < bl { 1 } else { 0 };
        break;
    }
    cfr != 0
}

/// Register all C API modules as globals and in package.loaded.
pub fn register_stubs(lua: &Lua) -> LuaResult<()> {
    let globals = lua.globals();
    let pkg_loaded: LuaTable = globals.get::<LuaTable>("package")?.get("loaded")?;

    let system = make_system(lua)?;
    insert(&globals, &pkg_loaded, "system", system)?;

    let process = make_process(lua)?;
    insert(&globals, &pkg_loaded, "process", process)?;

    let terminal = make_terminal(lua)?;
    insert(&globals, &pkg_loaded, "terminal", terminal)?;

    let terminal_buffer = terminal_buffer::make_module(lua)?;
    insert(&globals, &pkg_loaded, "terminal_buffer", terminal_buffer)?;

    let utf8extra = utf8extra::make_module(lua)?;
    insert(&globals, &pkg_loaded, "utf8extra", utf8extra)?;

    let renderer = make_renderer(lua)?;
    insert(&globals, &pkg_loaded, "renderer", renderer)?;

    let regex = make_regex(lua)?;
    insert(&globals, &pkg_loaded, "regex", regex)?;

    let renwindow = make_renwindow(lua)?;
    insert(&globals, &pkg_loaded, "renwindow", renwindow)?;

    let dm = dirmonitor::make_module(lua)?;
    insert(&globals, &pkg_loaded, "dirmonitor", dm)?;

    let md = make_markdown(lua)?;
    insert(&globals, &pkg_loaded, "markdown", md)?;

    let tokenizer = tokenizer::make_module(lua)?;
    insert(&globals, &pkg_loaded, "native_tokenizer", tokenizer)?;

    let project_fs = project_fs::make_module(lua)?;
    insert(&globals, &pkg_loaded, "project_fs", project_fs)?;

    let project_search = project_search::make_module(lua)?;
    insert(&globals, &pkg_loaded, "project_search", project_search)?;

    let project_manifest = project_manifest::make_module(lua)?;
    insert(&globals, &pkg_loaded, "project_manifest", project_manifest)?;

    let project_model = project_model::make_module(lua)?;
    insert(&globals, &pkg_loaded, "project_model", project_model)?;

    let doc_native = doc_native::make_module(lua)?;
    insert(&globals, &pkg_loaded, "doc_native", doc_native)?;

    let doc_layout = doc_layout::make_module(lua)?;
    insert(&globals, &pkg_loaded, "doc_layout", doc_layout)?;

    let symbol_index = symbol_index::make_module(lua)?;
    insert(&globals, &pkg_loaded, "symbol_index", symbol_index)?;

    let git_native = git_native::make_module(lua)?;
    insert(&globals, &pkg_loaded, "git_native", git_native)?;

    let picker = picker::make_module(lua)?;
    insert(&globals, &pkg_loaded, "picker", picker)?;

    let status_model = status_model::make_module(lua)?;
    insert(&globals, &pkg_loaded, "status_model", status_model)?;

    let node_model = node_model::make_module(lua)?;
    insert(&globals, &pkg_loaded, "node_model", node_model)?;

    let tree_model = tree_model::make_module(lua)?;
    insert(&globals, &pkg_loaded, "tree_model", tree_model)?;

    let lsp_manager = lsp_manager::make_module(lua)?;
    insert(&globals, &pkg_loaded, "lsp_manager", lsp_manager)?;

    let lsp_transport = lsp_transport::make_module(lua)?;
    insert(&globals, &pkg_loaded, "lsp_transport", lsp_transport)?;

    Ok(())
}

/// Register a table both as a Lua global and in package.loaded so both
/// `require "name"` and direct global access work — matching luaL_requiref.
fn insert(globals: &LuaTable, pkg_loaded: &LuaTable, name: &str, table: LuaTable) -> LuaResult<()> {
    globals.set(name, table.clone())?;
    pkg_loaded.set(name, table)?;
    Ok(())
}

type LazyModuleFactory = fn(&Lua) -> LuaResult<LuaTable>;

fn ensure_lazy_module(
    lua: &Lua,
    slot: &Arc<Mutex<Option<LuaRegistryKey>>>,
    factory: LazyModuleFactory,
) -> LuaResult<LuaTable> {
    let mut guard = slot.lock();
    if guard.is_none() {
        let module = factory(lua)?;
        *guard = Some(lua.create_registry_value(module.clone())?);
        return Ok(module);
    }
    lua.registry_value(guard.as_ref().unwrap())
}

fn make_lazy_dispatch(
    lua: &Lua,
    slot: Arc<Mutex<Option<LuaRegistryKey>>>,
    factory: LazyModuleFactory,
    method: &'static str,
) -> LuaResult<LuaFunction> {
    lua.create_function(
        move |lua, args: LuaMultiValue| -> LuaResult<LuaMultiValue> {
            let module = ensure_lazy_module(lua, &slot, factory)?;
            let func: LuaFunction = module.get(method)?;
            func.call(args)
        },
    )
}

// ── system module ─────────────────────────────────────────────────────────────

fn make_system(lua: &Lua) -> LuaResult<LuaTable> {
    let t = lua.create_table()?;

    // ── Filesystem (always available) ─────────────────────────────────────────

    t.set(
        "get_file_info",
        lua.create_function(|lua, path: String| -> LuaResult<LuaValue> {
            match std::fs::metadata(&path) {
                Ok(meta) => {
                    let info = lua.create_table()?;
                    info.set("type", if meta.is_dir() { "dir" } else { "file" })?;
                    info.set("size", meta.len())?;
                    let modified = meta
                        .modified()
                        .ok()
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_secs_f64())
                        .unwrap_or(0.0);
                    info.set("modified", modified)?;
                    Ok(LuaValue::Table(info))
                }
                Err(_) => Ok(LuaValue::Nil),
            }
        })?,
    )?;

    t.set(
        "absolute_path",
        lua.create_function(|lua, path: String| -> LuaResult<LuaValue> {
            match std::fs::canonicalize(&path) {
                Ok(p) => Ok(LuaValue::String(
                    lua.create_string(p.to_str().unwrap_or(""))?,
                )),
                Err(_) => Ok(LuaValue::Nil),
            }
        })?,
    )?;

    t.set(
        "list_dir",
        lua.create_function(|lua, path: String| -> LuaResult<LuaMultiValue> {
            match std::fs::read_dir(&path) {
                Ok(entries) => {
                    let names = lua.create_table()?;
                    let types = lua.create_table()?;
                    for (i, entry) in entries.flatten().enumerate() {
                        let idx = i as i64 + 1;
                        let name = entry.file_name();
                        names.raw_set(idx, name.to_str().unwrap_or(""))?;
                        // file_type() reads d_type from getdents64 on Linux — no extra syscall.
                        // For symlinks we follow with metadata() to get the target type.
                        let ftype = entry.file_type().ok();
                        let type_str = if ftype.map(|t| t.is_dir()).unwrap_or(false) {
                            "dir"
                        } else if ftype.map(|t| t.is_symlink()).unwrap_or(false) {
                            // follow symlink to distinguish dir symlinks
                            if std::fs::metadata(entry.path())
                                .map(|m| m.is_dir())
                                .unwrap_or(false)
                            {
                                "dir"
                            } else {
                                "file"
                            }
                        } else {
                            "file"
                        };
                        types.raw_set(idx, type_str)?;
                    }
                    Ok(LuaMultiValue::from_vec(vec![
                        LuaValue::Table(names),
                        LuaValue::Table(types),
                    ]))
                }
                Err(e) => Ok(LuaMultiValue::from_vec(vec![
                    LuaValue::Nil,
                    LuaValue::String(lua.create_string(e.to_string().as_bytes())?),
                ])),
            }
        })?,
    )?;

    t.set(
        "mkdir",
        lua.create_function(|lua, path: String| -> LuaResult<LuaMultiValue> {
            match std::fs::create_dir(&path) {
                Ok(()) => Ok(LuaMultiValue::from_vec(vec![LuaValue::Boolean(true)])),
                Err(e) => Ok(LuaMultiValue::from_vec(vec![
                    LuaValue::Boolean(false),
                    LuaValue::String(lua.create_string(e.to_string().as_bytes())?),
                ])),
            }
        })?,
    )?;

    t.set(
        "chdir",
        lua.create_function(|_, path: String| {
            std::env::set_current_dir(&path).map_err(|e| LuaError::RuntimeError(e.to_string()))
        })?,
    )?;

    // ── Timing ────────────────────────────────────────────────────────────────

    t.set(
        "get_time",
        lua.create_function(|_, ()| Ok(crate::time::elapsed_secs()))?,
    )?;

    t.set(
        "sleep",
        lua.create_function(|_, secs: f64| {
            std::thread::sleep(std::time::Duration::from_secs_f64(secs.max(0.0)));
            Ok(())
        })?,
    )?;

    // ── Sandbox detection ─────────────────────────────────────────────────────

    t.set("get_sandbox", lua.create_function(|_, ()| Ok("none"))?)?;

    // ── Native plugins — unimplemented ────────────────────────────────────────

    t.set(
        "load_native_plugin",
        lua.create_function(|_, path: String| {
            Err::<(), _>(LuaError::RuntimeError(format!(
                "native plugins not yet implemented: {path}"
            )))
        })?,
    )?;

    // ── Directory removal ──────────────────────────────────────────────────

    t.set(
        "rmdir",
        lua.create_function(|lua, path: String| -> LuaResult<LuaMultiValue> {
            match std::fs::remove_dir(&path) {
                Ok(()) => Ok(LuaMultiValue::from_vec(vec![LuaValue::Boolean(true)])),
                Err(e) => Ok(LuaMultiValue::from_vec(vec![
                    LuaValue::Boolean(false),
                    LuaValue::String(lua.create_string(e.to_string().as_bytes())?),
                ])),
            }
        })?,
    )?;

    // ── Process operations ─────────────────────────────────────────────────

    t.set(
        "exec",
        lua.create_function(|_, cmd: String| -> LuaResult<()> {
            let _ = std::process::Command::new("sh")
                .arg("-c")
                .arg(&cmd)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
            Ok(())
        })?,
    )?;

    t.set(
        "get_process_id",
        lua.create_function(|_, ()| -> LuaResult<u32> { Ok(std::process::id()) })?,
    )?;

    // ── Environment ────────────────────────────────────────────────────────

    t.set(
        "setenv",
        lua.create_function(|_, (key, val): (String, String)| -> LuaResult<bool> {
            // SAFETY: single-threaded at Lua call time; no concurrent env reads.
            unsafe { std::env::set_var(&key, &val) };
            Ok(true)
        })?,
    )?;

    t.set(
        "get_env",
        lua.create_function(|lua, name: String| -> LuaResult<LuaValue> {
            match std::env::var(&name) {
                Ok(v) => Ok(LuaValue::String(lua.create_string(v.as_bytes())?)),
                Err(_) => Ok(LuaValue::Nil),
            }
        })?,
    )?;

    // ── Fuzzy match / path compare ─────────────────────────────────────────

    t.set(
        "fuzzy_match",
        lua.create_function(
            |_, (haystack, needle, files): (String, String, Option<bool>)| -> LuaResult<LuaValue> {
                match fuzzy_match(&haystack, &needle, files.unwrap_or(false)) {
                    Some(score) => Ok(LuaValue::Integer(score)),
                    None => Ok(LuaValue::Nil),
                }
            },
        )?,
    )?;

    t.set(
        "path_compare",
        lua.create_function(
            |_, (p1, t1, p2, t2): (String, String, String, String)| -> LuaResult<bool> {
                Ok(path_compare(&p1, &t1, &p2, &t2))
            },
        )?,
    )?;

    // ── Filesystem extras ──────────────────────────────────────────────────

    t.set(
        "get_fs_type",
        lua.create_function(|_, _path: String| -> LuaResult<&'static str> { Ok("unknown") })?,
    )?;

    // ftruncate is only called on Windows; stub returns true on other platforms.
    t.set(
        "ftruncate",
        lua.create_function(|_, _: LuaMultiValue| -> LuaResult<bool> { Ok(true) })?,
    )?;

    // ── SDL-backed stubs — overridden in add_sdl_system_fns ───────────────

    // Clipboard: return empty string / no-op.
    t.set(
        "get_clipboard",
        lua.create_function(|lua, ()| -> LuaResult<LuaValue> {
            Ok(LuaValue::String(lua.create_string("")?))
        })?,
    )?;
    t.set(
        "set_clipboard",
        lua.create_function(|_, _text: String| -> LuaResult<()> { Ok(()) })?,
    )?;
    t.set(
        "get_primary_selection",
        lua.create_function(|lua, ()| -> LuaResult<LuaValue> {
            Ok(LuaValue::String(lua.create_string("")?))
        })?,
    )?;
    t.set(
        "set_primary_selection",
        lua.create_function(|_, _text: String| -> LuaResult<()> { Ok(()) })?,
    )?;

    // show_fatal_error: log to stderr as fallback.
    t.set(
        "show_fatal_error",
        lua.create_function(|_, (title, msg): (String, String)| -> LuaResult<()> {
            eprintln!("FATAL: {title}: {msg}");
            Ok(())
        })?,
    )?;

    // IME stubs — SDL2 has no ClearComposition equivalent.
    t.set(
        "clear_ime",
        lua.create_function(|_, _: LuaMultiValue| Ok(()))?,
    )?;

    // raise_window stub.
    t.set(
        "raise_window",
        lua.create_function(|_, _: LuaMultiValue| Ok(()))?,
    )?;

    // File-dialog stubs — return nil (user cancelled / unimplemented).
    for name in [
        "open_file_dialog",
        "save_file_dialog",
        "open_directory_dialog",
    ] {
        t.set(
            name,
            lua.create_function(|_, _: LuaMultiValue| -> LuaResult<LuaValue> {
                Ok(LuaValue::Nil)
            })?,
        )?;
    }
    // set_window_hit_test — borderless titlebar dragging (Phase 5).
    t.set(
        "set_window_hit_test",
        lua.create_function(|_, _: LuaMultiValue| Ok(()))?,
    )?;
    // text_input enable/disable — overridden in add_sdl_system_fns.
    t.set(
        "text_input",
        lua.create_function(|_, _: LuaMultiValue| Ok(()))?,
    )?;
    // set_text_input_rect — overridden in add_sdl_system_fns.
    t.set(
        "set_text_input_rect",
        lua.create_function(|_, _: LuaMultiValue| Ok(()))?,
    )?;

    // ── Window & event — SDL3 overrides stubs above ────────────────────────

    #[cfg(feature = "sdl")]
    add_sdl_system_fns(lua, &t)?;

    #[cfg(not(feature = "sdl"))]
    add_headless_system_fns(lua, &t)?;

    Ok(t)
}

// ── SDL3-backed window & event functions ──────────────────────────────────────

#[cfg(feature = "sdl")]
fn add_sdl_system_fns(lua: &Lua, t: &LuaTable) -> LuaResult<()> {
    use crate::window::PollResult;

    t.set(
        "poll_event",
        lua.create_function(|lua, ()| -> LuaResult<LuaMultiValue> {
            loop {
                match crate::window::poll_event() {
                    PollResult::Empty => return Ok(LuaMultiValue::new()),
                    PollResult::Skip => continue,
                    PollResult::Event(vals) => {
                        let mut mv = LuaMultiValue::new();
                        for v in vals {
                            mv.push_back(lua_event_val_to_lua(lua, v)?);
                        }
                        return Ok(mv);
                    }
                }
            }
        })?,
    )?;

    t.set(
        "wait_event",
        lua.create_function(|_, timeout: Option<f64>| -> LuaResult<bool> {
            Ok(crate::window::wait_event(timeout))
        })?,
    )?;

    // All window functions receive core.window as first arg (ignored — we have
    // a single global window via the thread-local SDL state).
    t.set(
        "get_window_size",
        lua.create_function(|_, _win: LuaValue| -> LuaResult<(i32, i32, i32, i32)> {
            Ok(crate::window::get_window_size())
        })?,
    )?;

    t.set(
        "set_window_size",
        lua.create_function(
            |_, (_win, w, h, x, y): (LuaValue, i32, i32, Option<i32>, Option<i32>)| {
                crate::window::set_window_size(w, h, x.unwrap_or(-1), y.unwrap_or(-1));
                Ok(())
            },
        )?,
    )?;

    t.set(
        "get_window_mode",
        lua.create_function(|_, _win: LuaValue| Ok(crate::window::get_window_mode()))?,
    )?;

    t.set(
        "set_window_mode",
        lua.create_function(|_, (_win, mode): (LuaValue, String)| {
            crate::window::set_window_mode(&mode);
            Ok(())
        })?,
    )?;

    t.set(
        "set_window_title",
        lua.create_function(|_, (_win, title): (LuaValue, String)| {
            crate::window::set_window_title(&title);
            Ok(())
        })?,
    )?;

    t.set(
        "set_window_bordered",
        lua.create_function(|_, (_win, bordered): (LuaValue, bool)| {
            crate::window::set_window_bordered(bordered);
            Ok(())
        })?,
    )?;

    t.set(
        "show_window",
        lua.create_function(|_, _: LuaMultiValue| {
            crate::window::show_window();
            Ok(())
        })?,
    )?;

    t.set(
        "window_has_focus",
        lua.create_function(|_, _win: LuaValue| Ok(crate::window::window_has_focus()))?,
    )?;

    t.set(
        "get_screen_size",
        lua.create_function(|_, ()| -> LuaResult<(i32, i32)> {
            Ok(crate::window::get_screen_size())
        })?,
    )?;

    t.set(
        "set_cursor",
        lua.create_function(|_, name: String| {
            crate::window::set_cursor(&name);
            Ok(())
        })?,
    )?;

    // ── Clipboard ──────────────────────────────────────────────────────────

    t.set(
        "get_clipboard",
        lua.create_function(|lua, ()| -> LuaResult<LuaValue> {
            let ptr = unsafe { sdl3_sys::everything::SDL_GetClipboardText() };
            if ptr.is_null() {
                return Ok(LuaValue::String(lua.create_string("")?));
            }
            let s = unsafe { std::ffi::CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned();
            unsafe { sdl3_sys::everything::SDL_free(ptr as *mut std::ffi::c_void) };
            Ok(LuaValue::String(lua.create_string(s.as_bytes())?))
        })?,
    )?;

    t.set(
        "set_clipboard",
        lua.create_function(|_, text: String| -> LuaResult<()> {
            if let Ok(cstr) = std::ffi::CString::new(text) {
                unsafe { sdl3_sys::everything::SDL_SetClipboardText(cstr.as_ptr()) };
            }
            Ok(())
        })?,
    )?;

    // ── Primary selection (X11) ────────────────────────────────────────────

    t.set(
        "get_primary_selection",
        lua.create_function(|lua, ()| -> LuaResult<LuaValue> {
            let ptr = unsafe { sdl3_sys::everything::SDL_GetPrimarySelectionText() };
            if ptr.is_null() {
                return Ok(LuaValue::String(lua.create_string("")?));
            }
            let s = unsafe { std::ffi::CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned();
            unsafe { sdl3_sys::everything::SDL_free(ptr as *mut std::ffi::c_void) };
            Ok(LuaValue::String(lua.create_string(s.as_bytes())?))
        })?,
    )?;

    t.set(
        "set_primary_selection",
        lua.create_function(|_, text: String| -> LuaResult<()> {
            if let Ok(cstr) = std::ffi::CString::new(text) {
                unsafe { sdl3_sys::everything::SDL_SetPrimarySelectionText(cstr.as_ptr()) };
            }
            Ok(())
        })?,
    )?;

    // ── Fatal error dialog ─────────────────────────────────────────────────

    t.set(
        "show_fatal_error",
        lua.create_function(|_, (title, msg): (String, String)| -> LuaResult<()> {
            let t = std::ffi::CString::new(title).unwrap_or_default();
            let m = std::ffi::CString::new(msg).unwrap_or_default();
            unsafe {
                sdl3_sys::everything::SDL_ShowSimpleMessageBox(
                    sdl3_sys::everything::SDL_MESSAGEBOX_ERROR,
                    t.as_ptr(),
                    m.as_ptr(),
                    std::ptr::null_mut(),
                );
            }
            Ok(())
        })?,
    )?;

    // ── Text input / IME ───────────────────────────────────────────────────

    t.set(
        "text_input",
        lua.create_function(|_, (_win, enable): (LuaValue, bool)| -> LuaResult<()> {
            let win = crate::window::get_raw_window();
            if enable {
                unsafe { sdl3_sys::everything::SDL_StartTextInput(win) };
            } else {
                unsafe { sdl3_sys::everything::SDL_StopTextInput(win) };
            }
            Ok(())
        })?,
    )?;

    t.set(
        "set_text_input_rect",
        lua.create_function(
            |_, (_win, x, y, w, h): (LuaValue, i32, i32, i32, i32)| -> LuaResult<()> {
                let rect = sdl3_sys::everything::SDL_Rect { x, y, w, h };
                let win = crate::window::get_raw_window();
                unsafe { sdl3_sys::everything::SDL_SetTextInputArea(win, &rect, 0) };
                Ok(())
            },
        )?,
    )?;

    // clear_ime: SDL3 has no SDL_ClearComposition equivalent.
    t.set(
        "clear_ime",
        lua.create_function(|_, _: LuaMultiValue| Ok(()))?,
    )?;

    // ── Window management ──────────────────────────────────────────────────

    t.set(
        "raise_window",
        lua.create_function(|_, _win: LuaValue| -> LuaResult<()> {
            crate::window::raise_window();
            Ok(())
        })?,
    )?;

    Ok(())
}

#[cfg(feature = "sdl")]
fn lua_event_val_to_lua(lua: &Lua, v: crate::window::LuaEventVal) -> LuaResult<LuaValue> {
    use crate::window::LuaEventVal::*;
    Ok(match v {
        Str(s) => LuaValue::String(lua.create_string(s)?),
        String(s) => LuaValue::String(lua.create_string(s.as_bytes())?),
        Int(n) => LuaValue::Integer(n),
        Float(f) => LuaValue::Number(f),
        Bool(b) => LuaValue::Boolean(b),
    })
}

// ── Headless stubs (no SDL3) ──────────────────────────────────────────────────

#[cfg(not(feature = "sdl"))]
fn add_headless_system_fns(lua: &Lua, t: &LuaTable) -> LuaResult<()> {
    // poll_event: always empty
    t.set(
        "poll_event",
        lua.create_function(|_, ()| -> LuaResult<LuaValue> { Ok(LuaValue::Nil) })?,
    )?;
    // wait_event: sleep the requested duration to avoid CPU spin
    t.set(
        "wait_event",
        lua.create_function(|_, timeout: Option<f64>| -> LuaResult<bool> {
            let ms = ((timeout.unwrap_or(0.1)).clamp(0.0, 1.0) * 1000.0) as u64;
            std::thread::sleep(std::time::Duration::from_millis(ms));
            Ok(false)
        })?,
    )?;
    t.set(
        "get_window_size",
        lua.create_function(|_, _: LuaValue| Ok((800i32, 600i32, 800i32, 600i32)))?,
    )?;
    t.set(
        "get_window_mode",
        lua.create_function(|_, _: LuaValue| Ok("normal"))?,
    )?;
    t.set(
        "window_has_focus",
        lua.create_function(|_, _: LuaValue| Ok(false))?,
    )?;
    t.set(
        "get_screen_size",
        lua.create_function(|_, ()| Ok((1920i32, 1080i32)))?,
    )?;
    for name in [
        "set_window_size",
        "set_window_mode",
        "set_window_title",
        "set_window_bordered",
        "show_window",
        "set_cursor",
    ] {
        t.set(name, lua.create_function(|_, _: LuaMultiValue| Ok(()))?)?;
    }
    Ok(())
}

// ── renderer module ───────────────────────────────────────────────────────────

#[cfg(feature = "sdl")]
fn make_renderer(lua: &Lua) -> LuaResult<LuaTable> {
    crate::renderer::make_renderer(lua)
}

/// Headless renderer stub — used when SDL is not compiled in.
#[cfg(not(feature = "sdl"))]
fn make_renderer(lua: &Lua) -> LuaResult<LuaTable> {
    lua.load(
        r#"
local Font = {}
Font.__index = Font
function Font.load(_path, _size, _opts) return setmetatable({}, Font) end
function Font:copy(_size) return setmetatable({}, Font) end
function Font:get_width(_text) return 0 end
function Font:get_height() return 14 end
function Font:get_size() return 14 end
function Font:set_size(_size) end
function Font:set_tab_size(_n) end
function Font:get_tab_size() return 4 end
local r = {}
r.font = Font
function r.begin_frame(_win) end
function r.end_frame() end
function r.set_clip_rect(_x, _y, _w, _h) end
function r.draw_rect(_x, _y, _w, _h, _color) end
function r.draw_text(_font, _text, _x, _y, _color) return _x end
return r
    "#,
    )
    .eval()
}

// ── regex module ──────────────────────────────────────────────────────────────

fn make_regex(lua: &Lua) -> LuaResult<LuaTable> {
    let t = lua.create_table()?;
    let slot = Arc::new(Mutex::new(None));

    t.set("ANCHORED", regex::ANCHORED)?;
    t.set("ENDANCHORED", regex::ENDANCHORED)?;
    t.set("NOTBOL", regex::NOTBOL)?;
    t.set("NOTEOL", regex::NOTEOL)?;
    t.set("NOTEMPTY", regex::NOTEMPTY)?;
    t.set("NOTEMPTY_ATSTART", regex::NOTEMPTY_ATSTART)?;

    let t_key = lua.create_registry_value(t.clone())?;
    let compile_slot = Arc::clone(&slot);
    t.set(
        "compile",
        lua.create_function(move |lua, args: LuaMultiValue| {
            let module = ensure_lazy_module(lua, &compile_slot, regex::make_module)?;
            let func: LuaFunction = module.get("compile")?;
            let compiled: LuaTable = func.call(args)?;
            let public: LuaTable = lua.registry_value(&t_key)?;
            compiled.set_metatable(Some(public))?;
            Ok(compiled)
        })?,
    )?;
    t.set(
        "cmatch",
        make_lazy_dispatch(lua, Arc::clone(&slot), regex::make_module, "cmatch")?,
    )?;
    t.set(
        "gmatch",
        make_lazy_dispatch(lua, Arc::clone(&slot), regex::make_module, "gmatch")?,
    )?;
    t.set(
        "gsub",
        make_lazy_dispatch(lua, slot, regex::make_module, "gsub")?,
    )?;

    Ok(t)
}

fn make_markdown(lua: &Lua) -> LuaResult<LuaTable> {
    let t = lua.create_table()?;
    let slot = Arc::new(Mutex::new(None));
    t.set(
        "parse",
        make_lazy_dispatch(lua, slot, markdown::make_module, "parse")?,
    )?;
    Ok(t)
}

// ── renwindow module ──────────────────────────────────────────────────────────

fn make_renwindow(lua: &Lua) -> LuaResult<LuaTable> {
    let t = lua.create_table()?;

    #[cfg(feature = "sdl")]
    {
        t.set(
            "_restore",
            lua.create_function(|lua, ()| -> LuaResult<LuaValue> {
                if crate::window::restore_window() {
                    Ok(LuaValue::Table(make_window_handle(lua)?))
                } else {
                    Ok(LuaValue::Nil)
                }
            })?,
        )?;
        t.set(
            "create",
            lua.create_function(|lua, title: Option<String>| -> LuaResult<LuaTable> {
                let title = title.as_deref().unwrap_or("Lite-Anvil");
                crate::window::create_window(title)
                    .map_err(|e| LuaError::RuntimeError(e.to_string()))?;
                make_window_handle(lua)
            })?,
        )?;
        t.set(
            "persist",
            lua.create_function(|_, ()| {
                crate::window::set_persistent(true);
                Ok(())
            })?,
        )?;
    }

    #[cfg(not(feature = "sdl"))]
    {
        let win: LuaTable = lua
            .load(
                r#"
local W = {}
W.__index = W
function W:get_size() return 800, 600 end
function W:get_content_scale() return 1.0 end
function W:_persist() end
return setmetatable({}, W)
            "#,
            )
            .eval()?;
        let win_clone = win.clone();
        t.set(
            "_restore",
            lua.create_function(move |_, ()| -> LuaResult<LuaValue> { Ok(LuaValue::Nil) })?,
        )?;
        t.set(
            "create",
            lua.create_function(move |_, _: LuaMultiValue| -> LuaResult<LuaTable> {
                Ok(win_clone.clone())
            })?,
        )?;
    }

    Ok(t)
}

/// Create the Lua table that Lua code holds as `core.window`.
/// It delegates :get_size() and :get_content_scale() to the window module.
#[cfg(feature = "sdl")]
fn make_window_handle(lua: &Lua) -> LuaResult<LuaTable> {
    let t = lua.create_table()?;
    t.set(
        "get_size",
        lua.create_function(|_, _: LuaMultiValue| -> LuaResult<(i32, i32)> {
            let (lw, lh, _pw, _ph) = crate::window::get_window_size();
            Ok((lw, lh))
        })?,
    )?;
    t.set(
        "get_content_scale",
        lua.create_function(|_, _: LuaMultiValue| -> LuaResult<f32> {
            let (pw, _ph) = crate::window::get_drawable_size();
            let (ww, _wh, _x, _y) = crate::window::get_window_size();
            Ok(if ww > 0 { pw as f32 / ww as f32 } else { 1.0 })
        })?,
    )?;
    t.set(
        "_persist",
        lua.create_function(|_, _: LuaMultiValue| {
            crate::window::set_persistent(true);
            Ok(())
        })?,
    )?;
    Ok(t)
}

// ── process module ────────────────────────────────────────────────────────────

#[cfg(unix)]
fn make_process(lua: &Lua) -> LuaResult<LuaTable> {
    process::make_module(lua)
}

#[cfg(not(unix))]
fn make_process(lua: &Lua) -> LuaResult<LuaTable> {
    let t = lua.create_table()?;
    t.set(
        "start",
        lua.create_function(|_, _: LuaMultiValue| -> LuaResult<LuaValue> {
            Err(LuaError::RuntimeError(
                "process.start is not supported on this platform".into(),
            ))
        })?,
    )?;
    Ok(t)
}

#[cfg(unix)]
fn make_terminal(lua: &Lua) -> LuaResult<LuaTable> {
    terminal::make_module(lua)
}

#[cfg(not(unix))]
fn make_terminal(lua: &Lua) -> LuaResult<LuaTable> {
    let t = lua.create_table()?;
    t.set(
        "spawn",
        lua.create_function(|_, _: LuaMultiValue| -> LuaResult<LuaValue> {
            Err(LuaError::RuntimeError(
                "terminal.spawn is not supported on this platform".into(),
            ))
        })?,
    )?;
    Ok(t)
}
