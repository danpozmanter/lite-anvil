use mlua::prelude::*;
use pcre2::bytes::Regex;

fn get_lines(lines: LuaTable) -> LuaResult<Vec<String>> {
    let mut out = Vec::new();
    for line in lines.sequence_values::<String>() {
        out.push(line?);
    }
    Ok(out)
}

fn position_offset(lines: &[String], mut line: usize, mut col: usize, offset: isize) -> (usize, usize) {
    let mut remaining = offset;
    if lines.is_empty() {
        return (1, 1);
    }
    line = line.clamp(1, lines.len());
    col = col.clamp(1, lines[line - 1].len().max(1));
    while remaining != 0 {
        if remaining > 0 {
            if col < lines[line - 1].len() {
                col += 1;
            } else if line < lines.len() {
                line += 1;
                col = 1;
            } else {
                break;
            }
            remaining -= 1;
        } else {
            if col > 1 {
                col -= 1;
            } else if line > 1 {
                line -= 1;
                col = lines[line - 1].len().max(1);
            } else {
                break;
            }
            remaining += 1;
        }
    }
    (line, col)
}

fn regex_find(line: &str, pattern: &str, no_case: bool, start_col: usize) -> Option<(usize, usize)> {
    let pat = if no_case {
        format!("(?i:{pattern})")
    } else {
        pattern.to_string()
    };
    let re = Regex::new(&pat).ok()?;
    let mut locs = re.capture_locations();
    re.captures_read_at(&mut locs, line.as_bytes(), start_col.saturating_sub(1))
        .ok()
        .flatten()?;
    let (s, e) = locs.get(0)?;
    Some((s + 1, e + 1))
}

fn replace_plain(text: &str, old: &str, new: &str) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut pos = 0usize;
    let mut count = 0usize;
    while let Some(off) = text[pos..].find(old) {
        let start = pos + off;
        out.push_str(&text[pos..start]);
        out.push_str(new);
        pos = start + old.len();
        count += 1;
    }
    out.push_str(&text[pos..]);
    (out, count)
}

fn replace_regex(text: &str, pattern: &str, new: &str) -> Result<(String, usize), String> {
    let re = Regex::new(pattern).map_err(|e| e.to_string())?;
    let mut out = String::with_capacity(text.len());
    let mut pos = 0usize;
    let mut count = 0usize;
    let bytes = text.as_bytes();
    let mut locs = re.capture_locations();
    while let Ok(Some(_)) = re.captures_read_at(&mut locs, bytes, pos) {
        let Some((s, e)) = locs.get(0) else {
            break;
        };
        out.push_str(&text[pos..s]);
        out.push_str(new);
        count += 1;
        if e > s {
            pos = e;
        } else {
            out.push_str(&text[s..s + 1]);
            pos = s + 1;
        }
        if pos >= text.len() {
            break;
        }
    }
    out.push_str(&text[pos..]);
    Ok((out, count))
}

pub fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    let module = lua.create_table()?;

    module.set(
        "position_offset",
        lua.create_function(|_, (lines, line, col, offset): (LuaTable, usize, usize, isize)| {
            let lines = get_lines(lines)?;
            Ok(position_offset(&lines, line, col, offset))
        })?,
    )?;

    module.set(
        "find",
        lua.create_function(
            |_, (lines, line, col, text, opts): (LuaTable, usize, usize, String, Option<LuaTable>)| {
                let lines = get_lines(lines)?;
                let no_case = opts
                    .as_ref()
                    .and_then(|t| t.get::<Option<bool>>("no_case").ok().flatten())
                    .unwrap_or(false);
                let regex = opts
                    .as_ref()
                    .and_then(|t| t.get::<Option<bool>>("regex").ok().flatten())
                    .unwrap_or(false);
                let reverse = opts
                    .as_ref()
                    .and_then(|t| t.get::<Option<bool>>("reverse").ok().flatten())
                    .unwrap_or(false);
                if reverse {
                    return Ok(LuaMultiValue::new());
                }
                for (idx, line_text) in lines.iter().enumerate().skip(line.saturating_sub(1)) {
                    let start_col = if idx + 1 == line { col } else { 1 };
                    let found = if regex {
                        regex_find(line_text, &text, no_case, start_col)
                    } else {
                        let hay = if no_case {
                            line_text.to_lowercase()
                        } else {
                            line_text.clone()
                        };
                        let needle = if no_case { text.to_lowercase() } else { text.clone() };
                        hay[start_col.saturating_sub(1)..]
                            .find(&needle)
                            .map(|off| {
                                let s = start_col + off;
                                let e = s + needle.len();
                                (s, e)
                            })
                    };
                    if let Some((s, e)) = found {
                        let end_line = if e > line_text.len() { idx + 2 } else { idx + 1 };
                        let end_col = if e > line_text.len() { 1 } else { e };
                        return Ok(LuaMultiValue::from_vec(vec![
                            LuaValue::Integer((idx + 1) as i64),
                            LuaValue::Integer(s as i64),
                            LuaValue::Integer(end_line as i64),
                            LuaValue::Integer(end_col as i64),
                        ]));
                    }
                }
                Ok(LuaMultiValue::new())
            },
        )?,
    )?;

    module.set(
        "replace",
        lua.create_function(|lua, (text, old, new, opts): (String, String, String, Option<LuaTable>)| {
            let regex = opts
                .as_ref()
                .and_then(|t| t.get::<Option<bool>>("regex").ok().flatten())
                .unwrap_or(false);
            let result = if regex {
                replace_regex(&text, &old, &new).map_err(LuaError::RuntimeError)?
            } else {
                replace_plain(&text, &old, &new)
            };
            let out = lua.create_table()?;
            out.set("text", result.0)?;
            out.set("count", result.1)?;
            Ok(out)
        })?,
    )?;

    Ok(module)
}
