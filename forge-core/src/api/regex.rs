use mlua::prelude::*;
use parking_lot::Mutex;
use pcre2::bytes::{Regex, RegexBuilder};
use std::sync::Arc;

// ── PCRE2 match-time option constants ────────────────────────────────────────

pub(crate) const ANCHORED: u32 = 0x80000000;
pub(crate) const ENDANCHORED: u32 = 0x20000000;
pub(crate) const NOTBOL: u32 = 0x00000001;
pub(crate) const NOTEOL: u32 = 0x00000002;
pub(crate) const NOTEMPTY: u32 = 0x00000004;
pub(crate) const NOTEMPTY_ATSTART: u32 = 0x00000008;

// ── UserData wrapper ──────────────────────────────────────────────────────────

pub struct RegexHandle(Arc<Regex>);

// SAFETY: pcre2::bytes::Regex uses internal thread-safe pools for match data.
unsafe impl Send for RegexHandle {}
unsafe impl Sync for RegexHandle {}

impl LuaUserData for RegexHandle {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(LuaMetaMethod::ToString, |_, _, ()| Ok("regex"));
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn compile_pattern(pattern: &str, flags: &str) -> LuaResult<Regex> {
    let mut b = RegexBuilder::new();
    b.utf(true).ucp(true);
    for c in flags.chars() {
        match c {
            'i' => {
                b.caseless(true);
            }
            'm' => {
                b.multi_line(true);
            }
            's' => {
                b.dotall(true);
            }
            _ => {}
        }
    }
    b.build(pattern)
        .map_err(|e| LuaError::RuntimeError(e.to_string()))
}

/// Accept a Lua string (compile on the fly), a RegexHandle userdata, or a
/// compiled-pattern table (C-compat: table[1] = RegexHandle).
fn arg_to_regex(val: LuaValue) -> LuaResult<Arc<Regex>> {
    match val {
        LuaValue::String(s) => {
            let p = s
                .to_str()
                .map_err(|e| LuaError::RuntimeError(e.to_string()))?;
            compile_pattern(&p, "").map(Arc::new)
        }
        LuaValue::UserData(ud) => {
            let h = ud.borrow::<RegexHandle>()?;
            Ok(Arc::clone(&h.0))
        }
        LuaValue::Table(t) => {
            // compile() returns a table with RegexHandle at key 1.
            let ud: LuaAnyUserData = t.raw_get(1)?;
            let h = ud.borrow::<RegexHandle>()?;
            Ok(Arc::clone(&h.0))
        }
        _ => Err(LuaError::RuntimeError(
            "expected string or compiled regex".into(),
        )),
    }
}

/// Execute a single PCRE2 match at `start` (0-based byte offset) and return a
/// flat Lua multivalue of 1-based (start, end_exclusive) pairs for group 0
/// followed by all capture groups.  Returns an empty multivalue on no match.
fn cmatch_at(_lua: &Lua, re: &Regex, subject: &[u8], start: usize) -> LuaResult<LuaMultiValue> {
    let mut locs = re.capture_locations();
    match re.captures_read_at(&mut locs, subject, start) {
        Ok(Some(_)) => {
            let n = re.captures_len() + 1; // groups 0 .. captures_len (inclusive)
            let mut mv = LuaMultiValue::new();
            for i in 0..n {
                match locs.get(i) {
                    Some((s, e)) => {
                        mv.push_back(LuaValue::Integer((s + 1) as i64)); // 1-based inclusive start
                        mv.push_back(LuaValue::Integer((e + 1) as i64)); // 1-based exclusive end; find_offsets subtracts 1
                    }
                    None => {
                        // Unmatched optional capture group.
                        mv.push_back(LuaValue::Integer(0));
                        mv.push_back(LuaValue::Integer(0));
                    }
                }
            }
            Ok(mv)
        }
        Ok(None) => Ok(LuaMultiValue::new()),
        Err(e) => Err(LuaError::RuntimeError(e.to_string())),
    }
}

/// Apply PCRE2 extended replacement string: `$$`→`$`, `$0`/`$n`/`${n}`→group.
fn apply_repl(repl: &[u8], subject: &[u8], locs: &pcre2::bytes::CaptureLocations) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < repl.len() {
        if repl[i] != b'$' || i + 1 >= repl.len() {
            out.push(repl[i]);
            i += 1;
            continue;
        }
        i += 1; // skip '$'
        if repl[i] == b'$' {
            out.push(b'$');
            i += 1;
        } else if repl[i] == b'{' {
            // ${n}
            if let Some(rel_end) = repl[i + 1..].iter().position(|&b| b == b'}') {
                let key = &repl[i + 1..i + 1 + rel_end];
                i += rel_end + 2; // skip '{', digits, '}'
                if let Some(n) = std::str::from_utf8(key)
                    .ok()
                    .and_then(|s| s.parse::<usize>().ok())
                {
                    if let Some((s, e)) = locs.get(n) {
                        out.extend_from_slice(&subject[s..e]);
                    }
                }
            } else {
                out.push(b'$');
                // leave i pointing at '{', will be processed as literal next round
            }
        } else if repl[i].is_ascii_digit() {
            let n = (repl[i] - b'0') as usize;
            i += 1;
            if let Some((s, e)) = locs.get(n) {
                out.extend_from_slice(&subject[s..e]);
            }
        } else {
            out.push(b'$');
            // don't advance i; next iteration processes repl[i] as literal
        }
    }
    out
}

// ── Module factory ────────────────────────────────────────────────────────────

pub fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    let t = lua.create_table()?;

    // Constants
    t.set("ANCHORED", ANCHORED)?;
    t.set("ENDANCHORED", ENDANCHORED)?;
    t.set("NOTBOL", NOTBOL)?;
    t.set("NOTEOL", NOTEOL)?;
    t.set("NOTEMPTY", NOTEMPTY)?;
    t.set("NOTEMPTY_ATSTART", NOTEMPTY_ATSTART)?;

    // compile(pattern [, flags]) → table{RegexHandle} with regex module as metatable.
    // Matches C backend: compiled patterns are tables that inherit regex.* via __index.
    let meta_key = lua.create_registry_value(t.clone())?;
    t.set(
        "compile",
        lua.create_function(move |lua, (pattern, flags): (String, Option<String>)| {
            let re = compile_pattern(&pattern, flags.as_deref().unwrap_or(""))?;
            let handle = RegexHandle(Arc::new(re));
            let compiled = lua.create_table()?;
            compiled.raw_set(1, handle)?;
            let meta: LuaTable = lua.registry_value(&meta_key)?;
            compiled.set_metatable(Some(meta))?;
            Ok(compiled)
        })?,
    )?;

    // cmatch(pattern_or_handle, str, offset, options) → flat (start, end+1) pairs
    // offset is 1-based; options currently ignored (no pcre2 match-flag API in crate).
    t.set(
        "cmatch",
        lua.create_function(
            |lua, (val, s, offset, _opts): (LuaValue, LuaString, Option<i64>, Option<u32>)| {
                let re = arg_to_regex(val)?;
                let bytes = s.as_bytes();
                let start = offset
                    .map(|o| (o - 1).clamp(0, bytes.len() as i64) as usize)
                    .unwrap_or(0);
                cmatch_at(lua, &re, &bytes, start)
            },
        )?,
    )?;

    // gmatch(pattern_or_handle, str [, offset]) → iterator function
    t.set(
        "gmatch",
        lua.create_function(
            |lua, (val, s, offset): (LuaValue, LuaString, Option<i64>)| {
                let re = arg_to_regex(val)?;
                let bytes: Vec<u8> = s.as_bytes().to_vec();
                let start = offset
                    .map(|o| (o - 1).clamp(0, bytes.len() as i64) as usize)
                    .unwrap_or(0);
                let pos = Arc::new(Mutex::new(start));
                let n_caps = re.captures_len();

                let iter = lua.create_function_mut(move |lua, ()| -> LuaResult<LuaMultiValue> {
                    let mut p = pos.lock();
                    if *p > bytes.len() {
                        return Ok(LuaMultiValue::new());
                    }
                    let mut locs = re.capture_locations();
                    match re.captures_read_at(&mut locs, &bytes, *p) {
                        Ok(Some(m)) => {
                            let ms = m.start();
                            let me = m.end();
                            *p = if me == ms { me + 1 } else { me };

                            let mut mv = LuaMultiValue::new();
                            if n_caps == 0 {
                                // No captures: yield whole match as string.
                                mv.push_back(LuaValue::String(lua.create_string(&bytes[ms..me])?));
                            } else {
                                for i in 1..=n_caps {
                                    match locs.get(i) {
                                        Some((s, e)) => {
                                            if s == e {
                                                mv.push_back(LuaValue::Integer((s + 1) as i64));
                                            } else {
                                                mv.push_back(LuaValue::String(
                                                    lua.create_string(&bytes[s..e])?,
                                                ));
                                            }
                                        }
                                        None => mv.push_back(LuaValue::Nil),
                                    }
                                }
                            }
                            Ok(mv)
                        }
                        _ => Ok(LuaMultiValue::new()),
                    }
                })?;
                Ok(iter)
            },
        )?,
    )?;

    // gsub(pattern_or_handle, str, repl, limit?) → (result_str, count)
    // repl uses PCRE2 extended substitution syntax: $0/$n/${n} for groups, $$ for $.
    t.set(
        "gsub",
        lua.create_function(
            |lua, (val, s, repl, limit): (LuaValue, LuaString, LuaString, Option<usize>)| {
                let re = arg_to_regex(val)?;
                let bytes = s.as_bytes();
                let repl_bytes = repl.as_bytes().to_vec();
                let limit = limit.unwrap_or(0); // 0 = unlimited

                let mut result: Vec<u8> = Vec::with_capacity(bytes.len());
                let mut count = 0usize;
                let mut pos = 0usize;

                loop {
                    if limit > 0 && count >= limit {
                        break;
                    }
                    if pos > bytes.len() {
                        break;
                    }
                    let mut locs = re.capture_locations();
                    match re.captures_read_at(&mut locs, &bytes, pos) {
                        Ok(Some(m)) => {
                            let ms = m.start();
                            let me = m.end();
                            result.extend_from_slice(&bytes[pos..ms]);
                            result.extend_from_slice(&apply_repl(&repl_bytes, &bytes, &locs));
                            count += 1;
                            if me == ms {
                                if pos < bytes.len() {
                                    result.push(bytes[pos]);
                                }
                                pos = me + 1;
                            } else {
                                pos = me;
                            }
                        }
                        _ => break,
                    }
                }
                result.extend_from_slice(&bytes[pos..]);
                Ok((lua.create_string(&result)?, count))
            },
        )?,
    )?;

    Ok(t)
}
