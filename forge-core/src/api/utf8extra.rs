use mlua::prelude::*;

/// Byte length of a single UTF-8 character starting at the given lead byte.
fn utf8_char_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b < 0xE0 {
        2
    } else if b < 0xF0 {
        3
    } else {
        4
    }
}

/// Returns true if the byte is a UTF-8 continuation byte.
fn is_continuation(b: u8) -> bool {
    (b & 0xC0) == 0x80
}

/// Returns the 1-based byte position of the n-th UTF-8 character (1-based).
fn charpos_impl(bytes: &[u8], n: i64) -> Option<usize> {
    if n == 0 {
        return Some(1);
    }
    let mut count = 0i64;
    for (i, &b) in bytes.iter().enumerate() {
        if !is_continuation(b) {
            count += 1;
            if count == n {
                return Some(i + 1);
            }
        }
    }
    None
}

/// Count total UTF-8 characters in a byte slice.
fn count_chars(bytes: &[u8]) -> i64 {
    bytes.iter().filter(|&&b| !is_continuation(b)).count() as i64
}

/// Count UTF-8 characters in `s[i-1..j]` (1-based byte indices, inclusive).
fn utf8_len(_lua: &Lua, (s, i, j): (LuaString, Option<i64>, Option<i64>)) -> LuaResult<i64> {
    let borrowed = s.as_bytes();
    let bytes: &[u8] = &borrowed;
    let len = bytes.len() as i64;
    let mut i = i.unwrap_or(1);
    let mut j = j.unwrap_or(len);
    if i < 0 {
        i = len + i + 1;
    }
    if j < 0 {
        j = len + j + 1;
    }
    i = i.max(1);
    j = j.min(len);
    if i > j {
        return Ok(0);
    }
    let count = bytes[(i as usize - 1)..j as usize]
        .iter()
        .filter(|&&b| !is_continuation(b))
        .count();
    Ok(count as i64)
}

/// Byte position of the n-th UTF-8 character.
fn utf8_charpos(_lua: &Lua, (s, n): (LuaString, i64)) -> LuaResult<LuaValue> {
    let borrowed = s.as_bytes();
    let bytes: &[u8] = &borrowed;
    match charpos_impl(bytes, n) {
        Some(pos) => Ok(LuaValue::Integer(pos as i64)),
        None => Ok(LuaValue::Nil),
    }
}

/// Substring by 1-based character indices (negative indices count from end).
fn utf8_sub(lua: &Lua, (s, i, j): (LuaString, i64, Option<i64>)) -> LuaResult<LuaString> {
    let borrowed = s.as_bytes();
    let bytes: &[u8] = &borrowed;
    let nchars = count_chars(bytes);
    let mut i = if i < 0 { nchars + i + 1 } else { i };
    let mut j = match j {
        Some(j) => {
            if j < 0 {
                nchars + j + 1
            } else {
                j
            }
        }
        None => nchars,
    };
    i = i.max(1);
    j = j.min(nchars);
    if i > j {
        return lua.create_string("");
    }
    let bi = match charpos_impl(bytes, i) {
        Some(p) => p - 1,
        None => return lua.create_string(""),
    };
    let bj_end = match charpos_impl(bytes, j + 1) {
        Some(p) => p - 1,
        None => bytes.len(),
    };
    lua.create_string(&bytes[bi..bj_end])
}

/// Reverse a UTF-8 string character-by-character.
fn utf8_reverse(lua: &Lua, s: LuaString) -> LuaResult<LuaString> {
    let borrowed = s.as_bytes();
    let bytes: &[u8] = &borrowed;
    let mut chars: Vec<&[u8]> = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        let clen = utf8_char_len(bytes[pos]);
        let end = (pos + clen).min(bytes.len());
        chars.push(&bytes[pos..end]);
        pos = end;
    }
    chars.reverse();
    let mut result = Vec::with_capacity(bytes.len());
    for chunk in &chars {
        result.extend_from_slice(chunk);
    }
    lua.create_string(&result)
}

/// Lowercase (delegates to Lua string.lower for locale consistency).
fn utf8_lower(lua: &Lua, s: LuaString) -> LuaResult<LuaString> {
    let string_table: LuaTable = lua.globals().get("string")?;
    let lower: LuaFunction = string_table.get("lower")?;
    lower.call(s)
}

/// Uppercase (delegates to Lua string.upper for locale consistency).
fn utf8_upper(lua: &Lua, s: LuaString) -> LuaResult<LuaString> {
    let string_table: LuaTable = lua.globals().get("string")?;
    let upper: LuaFunction = string_table.get("upper")?;
    upper.call(s)
}

/// Titlecase: uppercase first character, lowercase the rest.
fn utf8_title(lua: &Lua, s: LuaString) -> LuaResult<LuaString> {
    let borrowed = s.as_bytes();
    let bytes: &[u8] = &borrowed;
    if bytes.is_empty() {
        return lua.create_string(b"" as &[u8]);
    }
    let first_len = utf8_char_len(bytes[0]);
    let first_end = first_len.min(bytes.len());
    let string_table: LuaTable = lua.globals().get("string")?;
    let upper_fn: LuaFunction = string_table.get("upper")?;
    let lower_fn: LuaFunction = string_table.get("lower")?;
    let first: LuaString = lua.create_string(&bytes[..first_end])?;
    let rest: LuaString = lua.create_string(&bytes[first_end..])?;
    let upper_first: LuaString = upper_fn.call(first)?;
    let lower_rest: LuaString = lower_fn.call(rest)?;
    let uf_borrowed = upper_first.as_bytes();
    let lr_borrowed = lower_rest.as_bytes();
    let uf: &[u8] = &uf_borrowed;
    let lr: &[u8] = &lr_borrowed;
    let mut result = Vec::with_capacity(uf.len() + lr.len());
    result.extend_from_slice(uf);
    result.extend_from_slice(lr);
    lua.create_string(&result)
}

/// Case-fold (lowercase for simple folding).
fn utf8_fold(lua: &Lua, s: LuaString) -> LuaResult<LuaString> {
    utf8_lower(lua, s)
}

/// Case-insensitive comparison: returns -1, 0, or 1.
fn utf8_ncasecmp(lua: &Lua, (s1, s2): (LuaString, LuaString)) -> LuaResult<i64> {
    let string_table: LuaTable = lua.globals().get("string")?;
    let lower_fn: LuaFunction = string_table.get("lower")?;
    let l1: LuaString = lower_fn.call(s1)?;
    let l2: LuaString = lower_fn.call(s2)?;
    let b1 = l1.as_bytes();
    let b2 = l2.as_bytes();
    let r1: &[u8] = &b1;
    let r2: &[u8] = &b2;
    Ok(match r1.cmp(r2) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    })
}

/// Returns (end_byte_pos, codepoint) of the character after byte pos (0-based).
fn utf8_next(_lua: &Lua, (s, pos): (LuaString, Option<i64>)) -> LuaResult<LuaMultiValue> {
    let borrowed = s.as_bytes();
    let bytes: &[u8] = &borrowed;
    let pos = (pos.unwrap_or(0) + 1) as usize;
    if pos > bytes.len() || pos == 0 {
        return Ok(LuaMultiValue::new());
    }
    let idx = pos - 1;
    let b = bytes[idx];
    let clen = utf8_char_len(b);
    let cp = match clen {
        1 => b as u32,
        2 => {
            let b2 = bytes.get(idx + 1).copied().unwrap_or(0);
            ((b as u32 & 0x1F) << 6) | (b2 as u32 & 0x3F)
        }
        3 => {
            let b2 = bytes.get(idx + 1).copied().unwrap_or(0);
            let b3 = bytes.get(idx + 2).copied().unwrap_or(0);
            ((b as u32 & 0x0F) << 12) | ((b2 as u32 & 0x3F) << 6) | (b3 as u32 & 0x3F)
        }
        _ => {
            let b2 = bytes.get(idx + 1).copied().unwrap_or(0);
            let b3 = bytes.get(idx + 2).copied().unwrap_or(0);
            let b4 = bytes.get(idx + 3).copied().unwrap_or(0);
            ((b as u32 & 0x07) << 18)
                | ((b2 as u32 & 0x3F) << 12)
                | ((b3 as u32 & 0x3F) << 6)
                | (b4 as u32 & 0x3F)
        }
    };
    let end_pos = (pos + clen - 1) as i64;
    Ok(LuaMultiValue::from_vec(vec![
        LuaValue::Integer(end_pos),
        LuaValue::Integer(cp as i64),
    ]))
}

/// Convert \{XXXX} escape sequences to UTF-8 characters.
fn utf8_escape(lua: &Lua, s: LuaString) -> LuaResult<LuaString> {
    let string_table: LuaTable = lua.globals().get("string")?;
    let gsub: LuaFunction = string_table.get("gsub")?;
    let replacer = lua.create_function(|lua, hex: LuaString| -> LuaResult<LuaString> {
        let hex_str = hex.to_str()?;
        let tonumber: LuaFunction = lua.globals().get("tonumber")?;
        let cp: i64 = tonumber.call((hex_str.as_ref(), 16))?;
        let utf8_mod: LuaTable = lua.globals().get("utf8")?;
        let utf8_char_fn: LuaFunction = utf8_mod.get("char")?;
        utf8_char_fn.call(cp)
    })?;
    let result: LuaString = gsub.call((s, "\\\\{(%x+)}", replacer))?;
    Ok(result)
}

/// Insert a string at a character offset, or append if 2-arg form.
fn utf8_insert(lua: &Lua, args: LuaMultiValue) -> LuaResult<LuaString> {
    let mut iter = args.into_iter();
    let s: LuaString = match iter.next() {
        Some(LuaValue::String(s)) => s,
        _ => return Err(LuaError::runtime("expected string as first argument")),
    };
    let second = iter.next().unwrap_or(LuaValue::Nil);
    let third = iter.next().unwrap_or(LuaValue::Nil);

    let s_borrowed = s.as_bytes();
    let bytes: &[u8] = &s_borrowed;
    match &second {
        LuaValue::String(val) => {
            let v_borrowed = val.as_bytes();
            let val_bytes: &[u8] = &v_borrowed;
            let mut result = Vec::with_capacity(bytes.len() + val_bytes.len());
            result.extend_from_slice(bytes);
            result.extend_from_slice(val_bytes);
            lua.create_string(&result)
        }
        LuaValue::Integer(_) | LuaValue::Number(_) => {
            let offset = match &second {
                LuaValue::Integer(n) => *n,
                LuaValue::Number(n) => *n as i64,
                _ => unreachable!(),
            };
            let val: &LuaString = match &third {
                LuaValue::String(s) => s,
                _ => return Err(LuaError::runtime("expected string as third argument")),
            };
            let v_borrowed = val.as_bytes();
            let val_bytes: &[u8] = &v_borrowed;
            match charpos_impl(bytes, offset) {
                Some(bi) => {
                    let bi = bi - 1;
                    let mut result = Vec::with_capacity(bytes.len() + val_bytes.len());
                    result.extend_from_slice(&bytes[..bi]);
                    result.extend_from_slice(val_bytes);
                    result.extend_from_slice(&bytes[bi..]);
                    lua.create_string(&result)
                }
                None => {
                    let mut result = Vec::with_capacity(bytes.len() + val_bytes.len());
                    result.extend_from_slice(bytes);
                    result.extend_from_slice(val_bytes);
                    lua.create_string(&result)
                }
            }
        }
        _ => Err(LuaError::runtime(
            "expected string or number as second argument",
        )),
    }
}

/// Remove characters from start to fin (1-based character indices).
fn utf8_remove(
    lua: &Lua,
    (s, start, fin): (LuaString, i64, Option<i64>),
) -> LuaResult<LuaString> {
    let borrowed = s.as_bytes();
    let bytes: &[u8] = &borrowed;
    let fin = fin.unwrap_or(start);
    let bi = match charpos_impl(bytes, start) {
        Some(p) => p - 1,
        None => return lua.create_string(bytes),
    };
    let bj_next = match charpos_impl(bytes, fin + 1) {
        Some(p) => p - 1,
        None => bytes.len(),
    };
    let mut result = Vec::with_capacity(bytes.len() - (bj_next - bi));
    result.extend_from_slice(&bytes[..bi]);
    result.extend_from_slice(&bytes[bj_next..]);
    lua.create_string(&result)
}

/// Simplified width: treat every character as width 1.
fn utf8_width(_lua: &Lua, (s, _ambi, _i): (LuaString, LuaValue, LuaValue)) -> LuaResult<i64> {
    let borrowed = s.as_bytes();
    let bytes: &[u8] = &borrowed;
    Ok(count_chars(bytes))
}

/// Simplified widthindex: byte position of the w-th character.
fn utf8_widthindex(
    _lua: &Lua,
    (s, w, _ambi, _i): (LuaString, i64, LuaValue, LuaValue),
) -> LuaResult<LuaMultiValue> {
    let borrowed = s.as_bytes();
    let bytes: &[u8] = &borrowed;
    let pos = charpos_impl(bytes, w);
    Ok(LuaMultiValue::from_vec(vec![
        match pos {
            Some(p) => LuaValue::Integer(p as i64),
            None => LuaValue::Nil,
        },
        LuaValue::Integer(w),
    ]))
}

/// Builds the utf8extra module table with all functions.
pub fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    let m = lua.create_table()?;

    // Pattern functions: delegate to Lua string library (byte-oriented, correct for ASCII patterns)
    let string_table: LuaTable = lua.globals().get("string")?;
    m.set("find", string_table.get::<LuaValue>("find")?)?;
    m.set("match", string_table.get::<LuaValue>("match")?)?;
    m.set("gmatch", string_table.get::<LuaValue>("gmatch")?)?;
    m.set("gsub", string_table.get::<LuaValue>("gsub")?)?;
    m.set("byte", string_table.get::<LuaValue>("byte")?)?;

    // From Lua 5.4 built-in utf8 library
    let utf8_mod: LuaTable = lua.globals().get("utf8")?;
    m.set("char", utf8_mod.get::<LuaValue>("char")?)?;
    m.set("codepoint", utf8_mod.get::<LuaValue>("codepoint")?)?;
    m.set("codes", utf8_mod.get::<LuaValue>("codes")?)?;
    m.set("offset", utf8_mod.get::<LuaValue>("offset")?)?;
    m.set("charpattern", utf8_mod.get::<LuaValue>("charpattern")?)?;

    // Rust-implemented functions
    m.set("len", lua.create_function(utf8_len)?)?;
    m.set("charpos", lua.create_function(utf8_charpos)?)?;
    m.set("sub", lua.create_function(utf8_sub)?)?;
    m.set("reverse", lua.create_function(utf8_reverse)?)?;
    m.set("lower", lua.create_function(utf8_lower)?)?;
    m.set("upper", lua.create_function(utf8_upper)?)?;
    m.set("title", lua.create_function(utf8_title)?)?;
    m.set("fold", lua.create_function(utf8_fold)?)?;
    m.set("ncasecmp", lua.create_function(utf8_ncasecmp)?)?;
    m.set("next", lua.create_function(utf8_next)?)?;
    m.set("escape", lua.create_function(utf8_escape)?)?;
    m.set("insert", lua.create_function(utf8_insert)?)?;
    m.set("remove", lua.create_function(utf8_remove)?)?;
    m.set("width", lua.create_function(utf8_width)?)?;
    m.set("widthindex", lua.create_function(utf8_widthindex)?)?;

    Ok(m)
}
