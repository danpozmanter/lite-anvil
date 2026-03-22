use mlua::prelude::*;
use std::sync::Arc;

fn req(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    lua.globals().get::<LuaFunction>("require")?.call(name)
}

// ─── MANAGER HELPERS ──────────────────────────────────────────────────────────

fn mgr_uri_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push(char::from_digit((b >> 4) as u32, 16).unwrap().to_ascii_uppercase());
                out.push(char::from_digit((b & 0xf) as u32, 16).unwrap().to_ascii_uppercase());
            }
        }
    }
    out
}

fn mgr_path_to_uri(lua: &Lua, path: &str) -> LuaResult<String> {
    let common: LuaTable = req(lua, "core.common")?;
    let normalize: LuaFunction = common.get("normalize_path")?;
    let normalized: String = normalize.call(path)?;
    let mut normalized = normalized.replace('\\', "/");
    if !normalized.starts_with('/') {
        normalized.insert(0, '/');
    }
    Ok(format!("file://{}", mgr_uri_encode(&normalized)))
}

fn mgr_uri_to_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file://")?;
    let mut out = String::with_capacity(rest.len());
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = char::from(bytes[i + 1]).to_digit(16)?;
            let lo = char::from(bytes[i + 2]).to_digit(16)?;
            out.push((((hi << 4) | lo) as u8) as char);
            i += 3;
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    Some(out)
}

fn mgr_basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn mgr_byte_to_utf8_char(text: &str, col: usize) -> usize {
    let end = if col > 0 { col - 1 } else { 0 };
    let end = end.min(text.len());
    text[..end].chars().count()
}

fn mgr_utf8_char_to_byte(text: &str, char_idx: usize) -> usize {
    if char_idx == 0 {
        return 1;
    }
    let mut chars = text.char_indices();
    for _ in 0..char_idx {
        if chars.next().is_none() {
            return text.len() + 1;
        }
    }
    match chars.next() {
        Some((byte_pos, _)) => byte_pos + 1,
        None => text.len() + 1,
    }
}

fn mgr_content_to_text(v: &LuaValue) -> LuaResult<String> {
    match v {
        LuaValue::String(s) => Ok(s.to_str()?.to_owned()),
        LuaValue::Table(t) => {
            let kind: LuaValue = t.get("kind")?;
            if !matches!(kind, LuaValue::Nil) {
                let val: LuaValue = t.get("value")?;
                return mgr_content_to_text(&val);
            }
            let mut parts = Vec::new();
            for item in t.clone().sequence_values::<LuaValue>() {
                parts.push(mgr_content_to_text(&item?)?);
            }
            Ok(parts.join("\n"))
        }
        _ => Ok(String::new()),
    }
}

fn mgr_lsp_pos(lua: &Lua, doc: &LuaTable, line: i64, col: i64) -> LuaResult<LuaTable> {
    let lines: LuaTable = doc.get("lines")?;
    let text: String = lines.get::<String>(line).unwrap_or_else(|_| "\n".to_owned());
    let char_count = mgr_byte_to_utf8_char(&text, col as usize);
    let t = lua.create_table()?;
    t.set("line", line - 1)?;
    t.set("character", char_count as i64)?;
    Ok(t)
}

fn mgr_doc_pos(doc: &LuaTable, position: &LuaTable) -> LuaResult<(i64, i64)> {
    let lsp_line: i64 = position.get::<i64>("line").unwrap_or(0);
    let line = (lsp_line + 1).max(1);
    let lines: LuaTable = doc.get("lines")?;
    let text: String = lines.get::<String>(line).unwrap_or_else(|_| "\n".to_owned());
    let char_idx: i64 = position.get::<i64>("character").unwrap_or(0);
    let col = mgr_utf8_char_to_byte(&text, char_idx as usize) as i64;
    let sanitize: LuaFunction = doc.get("sanitize_position")?;
    let (sl, sc): (i64, i64) = sanitize.call((doc.clone(), line, col))?;
    Ok((sl, sc))
}

fn mgr_cap_supported(client: &LuaTable, cap: &str) -> LuaResult<bool> {
    let caps: LuaValue = client.get("capabilities")?;
    let caps = match caps {
        LuaValue::Table(t) => t,
        _ => return Ok(false),
    };
    let val: LuaValue = caps.get(cap)?;
    match val {
        LuaValue::Nil => Ok(false),
        LuaValue::Boolean(b) => Ok(b),
        _ => Ok(true),
    }
}

fn mgr_cap_config(client: &LuaTable, cap: &str) -> LuaResult<Option<LuaTable>> {
    let caps: LuaValue = client.get("capabilities")?;
    let caps = match caps {
        LuaValue::Table(t) => t,
        _ => return Ok(None),
    };
    let val: LuaValue = caps.get(cap)?;
    match val {
        LuaValue::Table(t) => Ok(Some(t)),
        _ => Ok(None),
    }
}

fn mgr_location_to_target(location: &LuaTable) -> LuaResult<(LuaValue, LuaValue)> {
    let target_uri: LuaValue = location.get("targetUri")?;
    if !matches!(target_uri, LuaValue::Nil) {
        let range: LuaValue = location
            .get::<LuaValue>("targetSelectionRange")
            .ok()
            .filter(|v| !matches!(v, LuaValue::Nil))
            .or_else(|| location.get::<LuaValue>("targetRange").ok().filter(|v| !matches!(v, LuaValue::Nil)))
            .or_else(|| location.get::<LuaValue>("range").ok())
            .unwrap_or(LuaValue::Nil);
        return Ok((target_uri, range));
    }
    let uri: LuaValue = location.get("uri")?;
    let range: LuaValue = location.get("range")?;
    Ok((uri, range))
}

fn mgr_make_location_items(lua: &Lua, locations: &LuaTable) -> LuaResult<LuaTable> {
    let items = lua.create_table()?;
    let common: LuaTable = req(lua, "core.common")?;
    let home_encode: LuaFunction = common.get("home_encode")?;
    let len = locations.raw_len();
    for i in 1..=len {
        let location: LuaTable = locations.get(i)?;
        let (uri_val, range_val) = mgr_location_to_target(&location)?;
        let uri = match &uri_val {
            LuaValue::String(s) => s.to_str()?.to_owned(),
            _ => continue,
        };
        let range = match range_val {
            LuaValue::Table(t) => t,
            _ => continue,
        };
        let path = match mgr_uri_to_path(&uri) {
            Some(p) => p,
            None => continue,
        };
        let start: LuaTable = range.get("start")?;
        let line: i64 = start.get::<i64>("line").unwrap_or(0) + 1;
        let col: i64 = start.get::<i64>("character").unwrap_or(0) + 1;
        let base = mgr_basename(&path);
        let encoded: String = home_encode.call(path.clone())?;
        let payload = lua.create_table()?;
        payload.set("uri", uri)?;
        payload.set("range", range)?;
        let item = lua.create_table()?;
        item.set("text", format!("{:03} {}:{}:{}", i, base, line, col))?;
        item.set("info", encoded)?;
        item.set("payload", payload)?;
        items.raw_set(items.raw_len() + 1, item)?;
    }
    Ok(items)
}

fn mgr_diagnostic_line_range(diagnostic: &LuaTable) -> LuaResult<Option<(i64, i64)>> {
    let range: LuaValue = diagnostic.get("range")?;
    let range = match range {
        LuaValue::Table(t) => t,
        _ => return Ok(None),
    };
    let start: LuaTable = range.get("start")?;
    let end_t: LuaTable = range.get("end").or_else(|_| range.get::<LuaTable>("start"))?;
    let start_line: i64 = start.get::<i64>("line").unwrap_or(0) + 1;
    let end_line: i64 = end_t.get::<i64>("line").unwrap_or(start_line - 1) + 1;
    Ok(Some((start_line, end_line)))
}

fn mgr_apply_text_edit(_lua: &Lua, doc: &LuaTable, edit: &LuaTable, move_cursor: bool) -> LuaResult<()> {
    let range: LuaTable = edit.get("range")?;
    let start: LuaTable = range.get("start")?;
    let end_t: LuaTable = range.get("end")?;
    let (sl, sc) = mgr_doc_pos(doc, &start)?;
    let (el, ec) = mgr_doc_pos(doc, &end_t)?;
    doc.get::<LuaFunction>("remove")?.call::<()>((doc.clone(), sl, sc, el, ec))?;
    let new_text: String = edit.get::<String>("newText").unwrap_or_default();
    if !new_text.is_empty() {
        doc.get::<LuaFunction>("insert")?.call::<()>((doc.clone(), sl, sc, new_text.clone()))?;
    }
    if move_cursor {
        let (end_line, end_col): (i64, i64) = if !new_text.is_empty() {
            doc.get::<LuaFunction>("position_offset")?
                .call::<(i64, i64)>((doc.clone(), sl, sc, new_text.len() as i64))?
        } else {
            (sl, sc)
        };
        doc.get::<LuaFunction>("set_selection")?
            .call::<()>((doc.clone(), end_line, end_col, end_line, end_col))?;
    }
    Ok(())
}

fn mgr_range_sort_desc(lua: &Lua, edits: &LuaTable) -> LuaResult<()> {
    let table_lib: LuaTable = lua.globals().get("table")?;
    let sort: LuaFunction = table_lib.get("sort")?;
    let cmp = lua.create_function(|_lua, (a, b): (LuaTable, LuaTable)| {
        let ar: LuaTable = a.get::<LuaTable>("range")?.get("start")?;
        let br: LuaTable = b.get::<LuaTable>("range")?.get("start")?;
        let al: i64 = ar.get::<i64>("line").unwrap_or(0);
        let bl: i64 = br.get::<i64>("line").unwrap_or(0);
        if al == bl {
            let ac: i64 = ar.get::<i64>("character").unwrap_or(0);
            let bc: i64 = br.get::<i64>("character").unwrap_or(0);
            return Ok(ac > bc);
        }
        Ok(al > bl)
    })?;
    sort.call::<()>((edits.clone(), cmp))?;
    Ok(())
}

fn mgr_apply_workspace_edit(lua: &Lua, edit: LuaValue) -> LuaResult<()> {
    let edit = match edit {
        LuaValue::Table(t) => t,
        _ => return Ok(()),
    };
    let grouped: Vec<(String, LuaTable)> = {
        let mut g: Vec<(String, LuaTable)> = Vec::new();
        if let LuaValue::Table(changes) = edit.get::<LuaValue>("changes")? {
            for pair in changes.pairs::<LuaValue, LuaTable>() {
                let (k, v) = pair?;
                if let LuaValue::String(s) = k {
                    let s_str = s.to_str()?;
                    if let Some(path) = mgr_uri_to_path(&s_str) {
                        g.push((path, v));
                    }
                }
            }
        }
        if let LuaValue::Table(doc_changes) = edit.get::<LuaValue>("documentChanges")? {
            for item in doc_changes.sequence_values::<LuaTable>() {
                let item = item?;
                let text_doc: LuaValue = item.get("textDocument")?;
                if let LuaValue::Table(td) = text_doc {
                    let uri: LuaValue = td.get("uri")?;
                    if let LuaValue::String(s) = uri {
                        let s_str = s.to_str()?;
                        if let Some(path) = mgr_uri_to_path(&s_str) {
                            if let LuaValue::Table(edits) = item.get::<LuaValue>("edits")? {
                                g.push((path, edits));
                            }
                        }
                    }
                }
            }
        }
        g
    };
    let core: LuaTable = req(lua, "core")?;
    for (abs_path, edits) in grouped {
        let doc: LuaTable = core.get::<LuaFunction>("open_doc")?.call(abs_path)?;
        mgr_range_sort_desc(lua, &edits)?;
        // Build native edits table
        let native_edits = lua.create_table()?;
        let edit_len = edits.raw_len();
        for i in 1..=edit_len {
            let item: LuaTable = edits.get(i)?;
            let range: LuaTable = item.get("range")?;
            let start: LuaTable = range.get("start")?;
            let end_t: LuaTable = range.get("end")?;
            let (sl, sc) = mgr_doc_pos(&doc, &start)?;
            let (el, ec) = mgr_doc_pos(&doc, &end_t)?;
            let ne = lua.create_table()?;
            ne.set("line1", sl)?;
            ne.set("col1", sc)?;
            ne.set("line2", el)?;
            ne.set("col2", ec)?;
            ne.set("text", item.get::<String>("newText").unwrap_or_default())?;
            native_edits.raw_set(i, ne)?;
        }
        let applied: bool = doc.get::<LuaFunction>("apply_edits")?.call((doc.clone(), native_edits)).unwrap_or(false);
        if !applied {
            for i in 1..=edit_len {
                let item: LuaTable = edits.get(i)?;
                mgr_apply_text_edit(lua, &doc, &item, false)?;
            }
        }
        core.get::<LuaFunction>("root_view")
            .and_then(|rv: LuaFunction| rv.call::<LuaTable>(()))
            .ok()
            .map(|rv: LuaTable| rv.get::<LuaFunction>("open_doc").and_then(|f| f.call::<LuaTable>((rv.clone(), doc.clone()))))
            .transpose()?;
        let rv: LuaTable = core.get("root_view")?;
        rv.get::<LuaFunction>("open_doc")?.call::<LuaTable>((rv, doc))?;
    }
    Ok(())
}

fn mgr_current_docview(lua: &Lua) -> LuaResult<Option<LuaTable>> {
    let core: LuaTable = req(lua, "core")?;
    let av: LuaValue = core.get("active_view")?;
    let av = match av {
        LuaValue::Table(t) => t,
        _ => return Ok(None),
    };
    let docview: LuaTable = req(lua, "core.docview")?;
    let is_fn: LuaFunction = av.get("is")?;
    let is_dv: bool = is_fn.call((av.clone(), docview)).unwrap_or(false);
    if is_dv { Ok(Some(av)) } else { Ok(None) }
}

fn mgr_get_doc_diagnostics(lua: &Lua, doc: &LuaTable) -> LuaResult<LuaTable> {
    let abs: LuaValue = doc.get("abs_filename")?;
    if matches!(abs, LuaValue::Nil) {
        return lua.create_table();
    }
    let abs: String = match abs {
        LuaValue::String(s) => s.to_str()?.to_owned(),
        _ => return lua.create_table(),
    };
    let uri = mgr_path_to_uri(lua, &abs)?;
    let native: LuaTable = req(lua, "lsp_manager")?;
    let get_diag: LuaFunction = native.get("get_diagnostics")?;
    match get_diag.call::<LuaValue>(uri)? {
        LuaValue::Table(t) => Ok(t),
        _ => lua.create_table(),
    }
}

fn mgr_get_sorted_doc_diagnostics(lua: &Lua, doc: &LuaTable) -> LuaResult<LuaTable> {
    let abs: LuaValue = doc.get("abs_filename")?;
    if matches!(abs, LuaValue::Nil) {
        return lua.create_table();
    }
    let abs: String = match abs {
        LuaValue::String(s) => s.to_str()?.to_owned(),
        _ => return lua.create_table(),
    };
    let uri = mgr_path_to_uri(lua, &abs)?;
    let native: LuaTable = req(lua, "lsp_manager")?;
    let f: LuaFunction = native.get("get_sorted_diagnostics")?;
    match f.call::<LuaValue>(uri)? {
        LuaValue::Table(t) => Ok(t),
        _ => lua.create_table(),
    }
}

fn mgr_diagnostic_color(lua: &Lua, severity: i64) -> LuaResult<LuaValue> {
    let style: LuaTable = req(lua, "core.style")?;
    let lint: Option<LuaTable> = match style.get::<LuaValue>("lint")? {
        LuaValue::Table(t) => Some(t),
        _ => None,
    };
    // Return the first non-nil value from the lint table key, falling back to style.
    let try_lint = |key: &str| -> Option<LuaValue> {
        lint.as_ref().and_then(|l| {
            l.get::<LuaValue>(key).ok().filter(|v| !matches!(v, LuaValue::Nil))
        })
    };
    match severity {
        1 => Ok(try_lint("error").map_or_else(|| style.get("error"), Ok)?),
        2 => Ok(try_lint("warning").map_or_else(|| style.get("warn"), Ok)?),
        3 => Ok(try_lint("info").map_or_else(|| style.get("accent"), Ok)?),
        _ => {
            if let Some(v) = try_lint("hint") { return Ok(v); }
            let good: LuaValue = style.get("good")?;
            if !matches!(good, LuaValue::Nil) { return Ok(good); }
            style.get("accent")
        }
    }
}

fn mgr_doc_pos_from_lsp(doc: &LuaTable, position: &LuaTable) -> LuaResult<(i64, i64)> {
    let lsp_line: i64 = position.get::<i64>("line").unwrap_or(0);
    let line = (lsp_line + 1).max(1);
    let lines: LuaTable = doc.get("lines")?;
    let text: String = lines.get::<String>(line).unwrap_or_else(|_| "\n".to_owned());
    let char_idx: i64 = position.get::<i64>("character").unwrap_or(0);
    let col = mgr_utf8_char_to_byte(&text, char_idx as usize) as i64;
    let sanitize: LuaFunction = doc.get("sanitize_position")?;
    let (sl, sc): (i64, i64) = sanitize.call((doc.clone(), line, col))?;
    Ok((sl, sc))
}

/// Wraps text into lines fitting max_width using the given font.
fn mgr_wrap_tooltip_lines(lua: &Lua, font: &LuaTable, text: &str, max_width: f64) -> LuaResult<LuaTable> {
    let get_width: LuaFunction = font.get("get_width")?;
    let lines = lua.create_table()?;
    let mut line_idx = 0i64;

    for raw_line in text.split('\n') {
        // Check for double-blank termination
        if raw_line.is_empty() {
            if line_idx > 0 {
                let prev: String = lines.get(line_idx).unwrap_or_default();
                if prev.is_empty() {
                    break;
                }
            }
            line_idx += 1;
            lines.raw_set(line_idx, "")?;
            continue;
        }
        let mut remaining = raw_line.to_owned();
        while !remaining.is_empty() {
            let w: f64 = get_width.call((font.clone(), remaining.clone()))?;
            if w <= max_width {
                line_idx += 1;
                lines.raw_set(line_idx, remaining.clone())?;
                break;
            }
            // Binary search for cut point
            let chars: Vec<char> = remaining.chars().collect();
            let mut cut = chars.len();
            while cut > 1 {
                let candidate: String = chars[..cut].iter().collect();
                let cw: f64 = get_width.call((font.clone(), candidate))?;
                if cw <= max_width {
                    break;
                }
                cut -= 1;
            }
            // Try to break at whitespace
            let candidate_str: String = chars[..cut].iter().collect();
            let split_pos = candidate_str.rfind(|c: char| c.is_whitespace());
            let actual_cut = if let Some(p) = split_pos { if p > 0 { p } else { cut } } else { cut };
            let line_str: String = chars[..actual_cut].iter().collect();
            let line_str = line_str.trim().to_owned();
            let line_str = if line_str.is_empty() {
                chars[..actual_cut.max(1)].iter().collect()
            } else {
                line_str
            };
            line_idx += 1;
            lines.raw_set(line_idx, line_str)?;
            let rest: String = chars[actual_cut..].iter().collect();
            remaining = rest.trim().to_owned();
        }
    }
    Ok(lines)
}

fn mgr_publish_diagnostics(lua: &Lua, mgr: &LuaTable, client: &LuaTable, params: &LuaTable) -> LuaResult<()> {
    let uri: String = params.get("uri")?;
    let doc_state: LuaTable = mgr.get("doc_state")?;
    let mut tracked_state: Option<LuaTable> = None;
    for pair in doc_state.clone().pairs::<LuaValue, LuaTable>() {
        let (_, state) = pair?;
        let state_uri: LuaValue = state.get("uri")?;
        if let LuaValue::String(s) = state_uri {
            if s.to_str()? == uri {
                tracked_state = Some(state);
                break;
            }
        }
    }
    let incoming_version: LuaValue = params.get("version")?;
    if let Some(ref state) = tracked_state {
        if let LuaValue::Integer(iv) = incoming_version {
            let sv: LuaValue = state.get("version")?;
            if let LuaValue::Integer(sv) = sv {
                if iv < sv {
                    return Ok(());
                }
            }
        }
    }
    let diagnostics: LuaValue = params.get::<LuaValue>("diagnostics").unwrap_or(LuaValue::Nil);
    let diag_table = match &diagnostics {
        LuaValue::Table(t) => t.clone(),
        _ => lua.create_table()?,
    };
    let native: LuaTable = req(lua, "lsp_manager")?;
    native.get::<LuaFunction>("publish_diagnostics")?.call::<()>((uri.clone(), params.get::<LuaValue>("version")?, diag_table.clone()))?;
    if let Some(state) = &tracked_state {
        let ver: LuaValue = params.get("version").unwrap_or(LuaValue::Nil);
        let ver = if matches!(ver, LuaValue::Nil) { state.get("version")? } else { ver };
        state.set("last_diagnostic_version", ver)?;
    }
    let path = mgr_uri_to_path(&uri);
    let label = path.as_deref().map(mgr_basename).unwrap_or(&uri).to_owned();
    let client_name: String = client.get("name")?;
    let diag_count = diag_table.raw_len();
    let core: LuaTable = req(lua, "core")?;
    let sv: LuaTable = core.get("status_view")?;
    let style: LuaTable = req(lua, "core.style")?;
    let accent: LuaValue = style.get("accent")?;
    sv.get::<LuaFunction>("show_message")?.call::<()>((sv, "!", accent, format!("LSP {}: {} diagnostic(s) for {}", client_name, diag_count, label)))?;
    core.set("redraw", true)?;
    Ok(())
}

fn mgr_apply_semantic_tokens(lua: &Lua, mgr: &LuaTable, doc: &LuaTable, client: &LuaTable, result: &LuaTable) -> LuaResult<()> {
    let data: LuaValue = result.get("data")?;
    let data = match data {
        LuaValue::Table(t) => t,
        _ => return Ok(()),
    };
    let doc_state: LuaTable = mgr.get("doc_state")?;
    let state: LuaValue = doc_state.get(doc.clone())?;
    let state = match state {
        LuaValue::Table(t) => t,
        _ => return Ok(()),
    };
    let client_caps: LuaValue = client.get("capabilities")?;
    let token_types: LuaTable = (|| -> LuaResult<LuaTable> {
        let caps = match client_caps {
            LuaValue::Table(t) => t,
            _ => return lua.create_table(),
        };
        let provider: LuaValue = caps.get("semanticTokensProvider")?;
        let provider = match provider {
            LuaValue::Table(t) => t,
            _ => return lua.create_table(),
        };
        let legend: LuaValue = provider.get("legend")?;
        let legend = match legend {
            LuaValue::Table(t) => t,
            _ => return lua.create_table(),
        };
        legend.get::<LuaTable>("tokenTypes").or_else(|_| lua.create_table())
    })()?;
    let native: LuaTable = req(lua, "lsp_manager")?;
    let lines: LuaTable = match native.get::<LuaFunction>("publish_semantic")?.call::<LuaValue>((token_types, data))? {
        LuaValue::Table(t) => t,
        _ => lua.create_table()?,
    };
    let semantic_lines: LuaTable = state.get("semantic_lines").unwrap_or_else(|_| lua.create_table().unwrap());
    let highlighter: LuaTable = doc.get("highlighter")?;
    let merge_line: LuaFunction = highlighter.get("merge_line")?;
    let get_sig: LuaFunction = highlighter.get("get_line_signature")?;
    let core: LuaTable = req(lua, "core")?;
    // Clear stale lines
    for pair in semantic_lines.clone().pairs::<i64, LuaValue>() {
        let (line_no, _) = pair?;
        let in_new: LuaValue = lines.get(line_no)?;
        if matches!(in_new, LuaValue::Nil) {
            merge_line.call::<()>((highlighter.clone(), line_no, LuaValue::Nil))?;
            semantic_lines.set(line_no, LuaValue::Nil)?;
            core.set("redraw", true)?;
        }
    }
    // Apply new lines
    let table_lib: LuaTable = lua.globals().get("table")?;
    let sort: LuaFunction = table_lib.get("sort")?;
    for pair in lines.pairs::<i64, LuaTable>() {
        let (line_no, positioned) = pair?;
        // Sort positioned tokens
        let cmp = lua.create_function(|_lua, (a, b): (LuaTable, LuaTable)| {
            let ap: i64 = a.get::<i64>("pos").unwrap_or(0);
            let bp: i64 = b.get::<i64>("pos").unwrap_or(0);
            if ap == bp {
                let al: i64 = a.get::<i64>("len").unwrap_or(0);
                let bl: i64 = b.get::<i64>("len").unwrap_or(0);
                return Ok(al < bl);
            }
            Ok(ap < bp)
        })?;
        sort.call::<()>((positioned.clone(), cmp))?;
        let before: LuaValue = get_sig.call((highlighter.clone(), line_no))?;
        merge_line.call::<()>((highlighter.clone(), line_no, positioned))?;
        semantic_lines.set(line_no, true)?;
        let after: LuaValue = get_sig.call((highlighter.clone(), line_no))?;
        // Compare signatures - if different, redraw
        let changed = match (&before, &after) {
            (LuaValue::String(a), LuaValue::String(b)) => a != b,
            (LuaValue::Nil, LuaValue::Nil) => false,
            _ => true,
        };
        if changed {
            core.set("redraw", true)?;
        }
    }
    state.set("semantic_lines", semantic_lines)?;
    Ok(())
}

// ─── MANAGER NAVIGATION HELPERS ───────────────────────────────────────────────

fn mgr_capture_view_location(lua: &Lua, view: &LuaTable) -> LuaResult<Option<LuaTable>> {
    let doc: LuaValue = view.get("doc")?;
    let doc = match doc { LuaValue::Table(t) => t, _ => return Ok(None) };
    let abs: LuaValue = doc.get("abs_filename")?;
    let abs_str = match abs {
        LuaValue::String(s) => s.to_str()?.to_owned(),
        _ => return Ok(None),
    };
    let sel: LuaMultiValue = doc.get::<LuaFunction>("get_selection")?.call(doc.clone())?;
    let mut iter = sel.into_iter();
    let line1 = match iter.next() { Some(LuaValue::Integer(n)) => n, _ => return Ok(None) };
    let col1  = match iter.next() { Some(LuaValue::Integer(n)) => n, _ => return Ok(None) };
    let line2 = match iter.next() { Some(LuaValue::Integer(n)) => n, _ => line1 };
    let col2  = match iter.next() { Some(LuaValue::Integer(n)) => n, _ => col1 };
    let loc = lua.create_table()?;
    loc.set("path",  abs_str)?;
    loc.set("line1", line1)?;
    loc.set("col1",  col1)?;
    loc.set("line2", line2)?;
    loc.set("col2",  col2)?;
    Ok(Some(loc))
}

fn mgr_open_captured_location(lua: &Lua, location: &LuaTable) -> LuaResult<bool> {
    let path: LuaValue = location.get("path")?;
    let path_str = match path { LuaValue::String(s) => s.to_str()?.to_owned(), _ => return Ok(false) };
    let core: LuaTable = req(lua, "core")?;
    let doc: LuaTable = core.get::<LuaFunction>("open_doc")?.call(path_str)?;
    let rv: LuaTable = core.get("root_view")?;
    let docview: LuaTable = rv.get::<LuaFunction>("open_doc")?.call((rv, doc.clone()))?;
    let line1: i64 = location.get("line1")?;
    let col1:  i64 = location.get("col1")?;
    let line2: i64 = location.get("line2")?;
    let col2:  i64 = location.get("col2")?;
    doc.get::<LuaFunction>("set_selection")?.call::<()>((doc, line1, col1, line2, col2))?;
    docview.get::<LuaFunction>("scroll_to_line")?.call::<()>((docview, line1, true, true))?;
    Ok(true)
}

/// Opens a file at the LSP uri+range, optionally pushing `history` onto the manager's location stack.
fn mgr_open_location(lua: &Lua, mgr: &LuaTable, uri: &str, range: &LuaTable, history: Option<LuaTable>) -> LuaResult<()> {
    let abs_path = match mgr_uri_to_path(uri) {
        Some(p) => p,
        None => {
            let core: LuaTable = req(lua, "core")?;
            core.get::<LuaFunction>("warn")?.call::<()>("LSP returned an unsupported location")?;
            return Ok(());
        }
    };
    if let Some(hist) = history {
        let loc_history: LuaTable = mgr.get("location_history")?;
        let len = loc_history.raw_len();
        let mut push = true;
        if len > 0 {
            if let Ok(LuaValue::Table(prev)) = loc_history.get::<LuaValue>(len) {
                let pp: LuaValue = prev.get("path")?;
                let pl: LuaValue = prev.get("line1")?;
                let pc: LuaValue = prev.get("col1")?;
                let hp: LuaValue = hist.get("path")?;
                let hl: LuaValue = hist.get("line1")?;
                let hc: LuaValue = hist.get("col1")?;
                if format!("{:?}{:?}{:?}", pp, pl, pc) == format!("{:?}{:?}{:?}", hp, hl, hc) {
                    push = false;
                }
            }
        }
        if push {
            loc_history.raw_set(len + 1, hist)?;
            if loc_history.raw_len() > 200 {
                lua.globals().get::<LuaTable>("table")?.get::<LuaFunction>("remove")?
                    .call::<LuaValue>((loc_history, 1))?;
            }
        }
    }
    let core: LuaTable = req(lua, "core")?;
    let doc: LuaTable = core.get::<LuaFunction>("open_doc")?.call(abs_path)?;
    let start: LuaTable = range.get("start")?;
    let end_t: LuaTable = range.get::<LuaTable>("end").or_else(|_| range.get("start"))?;
    let (line, col) = mgr_doc_pos(&doc, &start)?;
    let (end_line, end_col) = mgr_doc_pos(&doc, &end_t)?;
    let rv: LuaTable = core.get("root_view")?;
    let docview: LuaTable = rv.get::<LuaFunction>("open_doc")?.call((rv, doc.clone()))?;
    doc.get::<LuaFunction>("set_selection")?.call::<()>((doc, line, col, end_line, end_col))?;
    docview.get::<LuaFunction>("scroll_to_line")?.call::<()>((docview, line, true, true))?;
    Ok(())
}

/// Returns the LSP client for `doc` if a spec exists and the server is initialized with `capability`.
fn mgr_navigation_client(lua: &Lua, mgr: &LuaTable, doc: &LuaTable, capability: Option<&str>, action: &str) -> LuaResult<Option<LuaTable>> {
    let find_spec: LuaFunction = mgr.get("find_spec_for_doc")?;
    let spec_val: LuaValue = find_spec.call(doc.clone())?;
    if matches!(spec_val, LuaValue::Nil) {
        let syntax: LuaValue = doc.get("syntax")?;
        let label: String = if let LuaValue::Table(syn) = syntax {
            syn.get::<String>("name").unwrap_or_default()
        } else {
            doc.get::<LuaFunction>("get_name")?.call::<String>(doc.clone())?
        };
        let core: LuaTable = req(lua, "core")?;
        // navigation_config_hint
        let mut hints: Vec<String> = Vec::new();
        let globals = lua.globals();
        if let Ok(LuaValue::String(ud)) = globals.get::<LuaValue>("USERDIR") {
            let ud = ud.to_str()?.to_owned();
            let pathsep: String = globals.get("PATHSEP")?;
            let proj: LuaValue = core.get::<LuaFunction>("root_project")?.call(())?;
            if let LuaValue::Table(p) = proj {
                let proj_path: String = p.get("path")?;
                hints.push(format!("~/{}", "lsp.json"));
                hints.push(format!("{}{}{}", proj_path, pathsep, "lsp.json"));
            } else {
                hints.push(format!("{}{}{}", ud, pathsep, "lsp.json"));
            }
        }
        let hint = if hints.is_empty() { "lsp.json".to_owned() } else { hints.join(" or ") };
        core.get::<LuaFunction>("warn")?.call::<()>(
            format!("No LSP server configured for {}. Add a server in {}.", label, hint),
        )?;
        return Ok(None);
    }
    let open_doc: LuaFunction = mgr.get("open_doc")?;
    let client_val: LuaValue = open_doc.call(doc.clone())?;
    let client = match client_val { LuaValue::Table(t) => t, _ => return Ok(None) };
    if let Some(cap) = capability {
        let is_init: bool = client.get::<bool>("is_initialized").unwrap_or(false);
        if is_init && !mgr_cap_supported(&client, cap)? {
            let core: LuaTable = req(lua, "core")?;
            let name: String = client.get("name")?;
            core.get::<LuaFunction>("warn")?.call::<()>(
                format!("LSP server {} does not support {}", name, action),
            )?;
            return Ok(None);
        }
    }
    Ok(Some(client))
}

/// Displays a fuzzy-picker over `items` using command_view; calls `on_submit` with the chosen item.
fn mgr_pick_from_list(lua: &Lua, label: &str, items: LuaTable, on_submit: LuaFunction) -> LuaResult<()> {
    if items.raw_len() == 0 {
        let core: LuaTable = req(lua, "core")?;
        core.get::<LuaFunction>("warn")?.call::<()>(format!("{}: no results", label))?;
        return Ok(());
    }
    let native_picker: LuaTable = req(lua, "picker")?;
    let rank_items: LuaFunction = native_picker.get("rank_items")?;
    let items_key = Arc::new(lua.create_registry_value(items)?);
    let rank_key  = Arc::new(lua.create_registry_value(rank_items)?);
    let submit_key = Arc::new(lua.create_registry_value(on_submit)?);
    let items_key2 = Arc::clone(&items_key);
    let rank_key2  = Arc::clone(&rank_key);
    let submit_key2 = Arc::clone(&submit_key);
    let rank_key3 = Arc::clone(&rank_key);
    let items_key3 = Arc::clone(&items_key);
    let opts = lua.create_table()?;
    opts.set("submit", lua.create_function(move |lua, (text, item): (String, LuaValue)| {
        let items: LuaTable = lua.registry_value(&items_key2)?;
        let rank: LuaFunction = lua.registry_value(&rank_key2)?;
        let on_sub: LuaFunction = lua.registry_value(&submit_key2)?;
        let selected = if !matches!(item, LuaValue::Nil) {
            item
        } else {
            let ranked: LuaTable = rank.call((items, text, "text"))?;
            ranked.get(1).unwrap_or(LuaValue::Nil)
        };
        if let LuaValue::Table(sel) = selected {
            let payload: LuaValue = sel.get("payload")?;
            let arg = if matches!(payload, LuaValue::Nil) { LuaValue::Table(sel) } else { payload };
            on_sub.call::<()>(arg)?;
        }
        Ok(())
    })?)?;
    opts.set("suggest", lua.create_function(move |lua, text: String| {
        let items: LuaTable = lua.registry_value(&items_key3)?;
        let rank: LuaFunction = lua.registry_value(&rank_key3)?;
        rank.call::<LuaTable>((items, text, "text"))
    })?)?;
    let core: LuaTable = req(lua, "core")?;
    let cv: LuaTable = core.get("command_view")?;
    cv.get::<LuaFunction>("enter")?.call::<()>((cv, label.to_owned(), opts))?;
    Ok(())
}

fn mgr_flatten_document_symbols(lua: &Lua, symbols: &LuaTable, uri: &str, out: &LuaTable, prefix: &str) -> LuaResult<()> {
    let len = symbols.raw_len();
    for i in 1..=len {
        let symbol: LuaTable = symbols.get(i)?;
        let name: String = symbol.get::<String>("name").unwrap_or_else(|_| "?".to_owned());
        let display_name = if prefix.is_empty() { name.clone() } else { format!("{} / {}", prefix, name) };
        let range_val: LuaValue = symbol.get::<LuaValue>("selectionRange")
            .ok().filter(|v| !matches!(v, LuaValue::Nil))
            .or_else(|| symbol.get::<LuaValue>("range").ok().filter(|v| !matches!(v, LuaValue::Nil)))
            .or_else(|| {
                symbol.get::<LuaTable>("location").ok()
                    .and_then(|loc| loc.get::<LuaValue>("range").ok())
                    .filter(|v| !matches!(v, LuaValue::Nil))
            })
            .unwrap_or(LuaValue::Nil);
        let symbol_uri: String = symbol.get::<String>("uri")
            .or_else(|_| symbol.get::<LuaTable>("location").and_then(|loc| loc.get("uri")))
            .unwrap_or_else(|_| uri.to_owned());
        if !matches!(range_val, LuaValue::Nil) {
            let detail: LuaValue = symbol.get("detail").unwrap_or(LuaValue::Nil);
            let kind: LuaValue = symbol.get("kind").unwrap_or(LuaValue::Nil);
            let info = match detail {
                LuaValue::Nil => format!("{:?}", kind),
                LuaValue::String(s) => s.to_str()?.to_owned(),
                other => format!("{:?}", other),
            };
            let payload = lua.create_table()?;
            payload.set("uri", symbol_uri.clone())?;
            payload.set("range", range_val)?;
            let item = lua.create_table()?;
            item.set("text", display_name.clone())?;
            item.set("info", info)?;
            item.set("payload", payload)?;
            out.raw_set(out.raw_len() + 1, item)?;
        }
        if let Ok(LuaValue::Table(children)) = symbol.get::<LuaValue>("children") {
            mgr_flatten_document_symbols(lua, &children, &symbol_uri, out, &display_name)?;
        }
    }
    Ok(())
}

fn apply_code_action_inner(lua: &Lua, client: &LuaTable, action: &LuaTable) -> LuaResult<()> {
    let disabled: LuaValue = action.get("disabled").unwrap_or(LuaValue::Nil);
    if !matches!(disabled, LuaValue::Nil) {
        let reason = match &disabled {
            LuaValue::Table(t) => t.get::<String>("reason").unwrap_or_default(),
            _ => String::new(),
        };
        let msg = if reason.is_empty() { "LSP action unavailable".to_owned() } else { format!("LSP action unavailable: {}", reason) };
        let core: LuaTable = req(lua, "core")?;
        core.get::<LuaFunction>("warn")?.call::<()>(msg)?;
        return Ok(());
    }
    let provider = mgr_cap_config(client, "codeActionProvider")?;
    let needs_resolve = provider.map(|p| {
        p.get::<bool>("resolveProvider").unwrap_or(false)
    }).unwrap_or(false);
    let has_data = !matches!(action.get::<LuaValue>("data").unwrap_or(LuaValue::Nil), LuaValue::Nil);
    if needs_resolve && has_data {
        let client_c = client.clone();
        let action_c = action.clone();
        let cb = lua.create_function(move |lua, (resolved, err): (LuaValue, LuaValue)| {
            let resolved = if matches!(err, LuaValue::Nil) {
                match resolved { LuaValue::Table(t) => t, _ => action_c.clone() }
            } else { action_c.clone() };
            if let LuaValue::Table(edit) = resolved.get::<LuaValue>("edit").unwrap_or(LuaValue::Nil) {
                mgr_apply_workspace_edit(lua, LuaValue::Table(edit))?;
            }
            if let LuaValue::Table(cmd) = resolved.get::<LuaValue>("command").unwrap_or(LuaValue::Nil) {
                let command: String = cmd.get("command").unwrap_or_default();
                if !command.is_empty() {
                    let params = lua.create_table()?;
                    params.set("command", command)?;
                    params.set("arguments", cmd.get::<LuaValue>("arguments").unwrap_or(LuaValue::Nil))?;
                    let _ = client_c.get::<LuaFunction>("request")?.call::<LuaValue>((client_c.clone(), "workspace/executeCommand", params, LuaValue::Nil));
                }
            }
            Ok(())
        })?;
        client.get::<LuaFunction>("request")?.call::<()>((client.clone(), "codeAction/resolve", action.clone(), cb))?;
    } else {
        if let LuaValue::Table(edit) = action.get::<LuaValue>("edit").unwrap_or(LuaValue::Nil) {
            mgr_apply_workspace_edit(lua, LuaValue::Table(edit))?;
        }
        if let LuaValue::Table(cmd) = action.get::<LuaValue>("command").unwrap_or(LuaValue::Nil) {
            let command: String = cmd.get("command").unwrap_or_default();
            if !command.is_empty() {
                let params = lua.create_table()?;
                params.set("command", command)?;
                params.set("arguments", cmd.get::<LuaValue>("arguments").unwrap_or(LuaValue::Nil))?;
                let _ = client.get::<LuaFunction>("request")?.call::<LuaValue>((client.clone(), "workspace/executeCommand", params, LuaValue::Nil));
            }
        }
    }
    Ok(())
}

// ─── MANAGER MODULE ───────────────────────────────────────────────────────────

/// Pure-Rust replacement for LSP_MANAGER_SOURCE.
#[allow(clippy::too_many_lines)]
fn init_manager_module(lua: &Lua) -> LuaResult<LuaValue> {
    let setmetatable: LuaFunction = lua.globals().get("setmetatable")?;
    let mgr = lua.create_table()?;
    mgr.set("config_path",  LuaValue::Nil)?;
    mgr.set("config_paths", lua.create_table()?)?;
    mgr.set("raw_config",   lua.create_table()?)?;
    mgr.set("specs",        lua.create_table()?)?;
    mgr.set("clients",      lua.create_table()?)?;
    {
        let t  = lua.create_table()?;
        let mt = lua.create_table()?;
        mt.set("__mode", "k")?;
        mgr.set("doc_state", setmetatable.call::<LuaTable>((t, mt))?)?;
    }
    mgr.set("diagnostics",                     lua.create_table()?)?;
    mgr.set("location_history",                lua.create_table()?)?;
    mgr.set("semantic_refresh_thread_started", false)?;

    let mgr_key = Arc::new(lua.create_registry_value(mgr.clone())?);

    // ── reload_config ──────────────────────────────────────────────────────────
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("reload_config", lua.create_function(move |lua, ()| {
            let m: LuaTable = lua.registry_value(&mk)?;
            let core: LuaTable = req(lua, "core")?;
            let project: LuaValue = core.get::<LuaFunction>("root_project")?.call(())?;
            let setmt: LuaFunction = lua.globals().get("setmetatable")?;
            m.set("raw_config", lua.create_table()?)?;
            m.set("specs",      lua.create_table()?)?;
            m.set("clients",    lua.create_table()?)?;
            m.set("diagnostics", lua.create_table()?)?;
            {
                let t  = lua.create_table()?;
                let mt = lua.create_table()?;
                mt.set("__mode", "k")?;
                m.set("doc_state", setmt.call::<LuaTable>((t, mt))?)?;
            }
            let config_paths = lua.create_table()?;
            let globals = lua.globals();
            let pathsep: String = globals.get("PATHSEP")?;
            if let Ok(LuaValue::String(ud)) = globals.get::<LuaValue>("USERDIR") {
                let ud = ud.to_str()?.to_owned();
                config_paths.raw_set(config_paths.raw_len() + 1,
                    format!("{}{}{}", ud, pathsep, "lsp.json"))?;
            }
            if let LuaValue::Table(proj) = &project {
                let proj_path: String = proj.get("path")?;
                config_paths.raw_set(config_paths.raw_len() + 1,
                    format!("{}{}{}", proj_path, pathsep, "lsp.json"))?;
            }
            m.set("config_paths", config_paths.clone())?;
            let last_idx = config_paths.raw_len();
            let last: LuaValue = if last_idx > 0 { config_paths.get(last_idx)? } else { LuaValue::Nil };
            m.set("config_path", last)?;
            let native: LuaTable = req(lua, "lsp_manager")?;
            let reload: LuaFunction = native.get("reload_config")?;
            match reload.call::<i64>(config_paths) {
                Ok(n)  => Ok(n > 0),
                Err(e) => {
                    core.get::<LuaFunction>("warn")?.call::<()>(
                        format!("Failed to parse lsp.json: {}", e))?;
                    Ok(false)
                }
            }
        })?)?;
    }

    // ── start_semantic_refresh_loop ────────────────────────────────────────────
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("start_semantic_refresh_loop", lua.create_function(move |lua, ()| {
            let m: LuaTable = lua.registry_value(&mk)?;
            if m.get::<bool>("semantic_refresh_thread_started").unwrap_or(false) {
                return Ok(());
            }
            m.set("semantic_refresh_thread_started", true)?;
            let mk2 = Arc::clone(&mk);
            // One tick of semantic refresh work; returns yield delay.
            // coroutine.yield cannot be called from a Rust C function (lua_call has no
            // continuation), so the loop+yield live in a thin Lua wrapper below.
            let tick = lua.create_function(move |lua, ()| -> LuaResult<f64> {
                let m: LuaTable = lua.registry_value(&mk2)?;
                let system: LuaTable = lua.globals().get("system")?;
                let native: LuaTable = req(lua, "lsp_manager")?;
                let take_due: LuaFunction = native.get("take_due_semantic")?;
                let req_sem: LuaFunction = m.get("request_semantic_tokens")?;
                let now: f64 = system.get::<LuaFunction>("get_time")?.call(())?;
                let due: LuaTable = match take_due.call::<LuaValue>(now)? {
                    LuaValue::Table(t) => t,
                    _ => lua.create_table()?,
                };
                for item in due.sequence_values::<String>() {
                    let uri = item?;
                    let doc_state: LuaTable = m.get("doc_state")?;
                    for pair in doc_state.pairs::<LuaValue, LuaTable>() {
                        let (doc_val, state) = pair?;
                        let state_uri: LuaValue = state.get("uri")?;
                        if let LuaValue::String(s) = state_uri {
                            if s.to_str()? == uri {
                                req_sem.call::<()>(doc_val)?;
                                break;
                            }
                        }
                    }
                }
                Ok(0.1f64)
            })?;
            // Lua function: loops and yields — only Lua functions may yield in Lua 5.4.
            let thread_fn: LuaFunction = lua.load(
                "local t = ...; return function() while true do coroutine.yield(t()) end end"
            ).call::<LuaFunction>(tick)?;
            let core: LuaTable = req(lua, "core")?;
            core.get::<LuaFunction>("add_thread")?.call::<()>(thread_fn)
        })?)?;
    }

    // ── find_spec_for_doc ──────────────────────────────────────────────────────
    {
        mgr.set("find_spec_for_doc", lua.create_function(|lua, doc: LuaTable| {
            let abs: LuaValue = doc.get("abs_filename")?;
            if matches!(abs, LuaValue::Nil) { return Ok(LuaMultiValue::new()); }
            let syntax: LuaValue = doc.get("syntax")?;
            let syn_name: String = match syntax {
                LuaValue::Table(t) => t.get::<String>("name").unwrap_or_default(),
                _ => return Ok(LuaMultiValue::new()),
            };
            if syn_name.is_empty() { return Ok(LuaMultiValue::new()); }
            let core: LuaTable = req(lua, "core")?;
            let project: LuaValue = core.get::<LuaFunction>("root_project")?.call(())?;
            let project = match project { LuaValue::Table(t) => t, _ => return Ok(LuaMultiValue::new()) };
            let proj_path: String = project.get("path")?;
            let abs_str: String = match abs { LuaValue::String(s) => s.to_str()?.to_owned(), _ => return Ok(LuaMultiValue::new()) };
            let native: LuaTable = req(lua, "lsp_manager")?;
            let find_spec: LuaFunction = native.get("find_spec")?;
            let spec: LuaValue = find_spec.call((syn_name.to_lowercase(), abs_str, proj_path))?;
            match spec {
                LuaValue::Table(t) => {
                    let root_dir: LuaValue = t.get("root_dir")?;
                    Ok(LuaMultiValue::from_vec(vec![LuaValue::Table(t), root_dir]))
                }
                _ => Ok(LuaMultiValue::new()),
            }
        })?)?;
    }

    // ── ensure_client ──────────────────────────────────────────────────────────
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("ensure_client", lua.create_function(move |lua, doc: LuaTable| {
            let m: LuaTable = lua.registry_value(&mk)?;
            let find_spec: LuaFunction = m.get("find_spec_for_doc")?;
            let result: LuaMultiValue = find_spec.call(doc.clone())?;
            let mut iter = result.into_iter();
            let spec = match iter.next() { Some(LuaValue::Table(t)) => t, _ => return Ok(LuaValue::Nil) };
            let root_dir: String = match iter.next() { Some(LuaValue::String(s)) => s.to_str()?.to_owned(), _ => return Ok(LuaValue::Nil) };
            let spec_name: String = spec.get("name")?;
            let key = format!("{}@{}", spec_name, root_dir);
            let clients: LuaTable = m.get("clients")?;
            if let Ok(LuaValue::Table(existing)) = clients.get::<LuaValue>(key.clone()) {
                return Ok(LuaValue::Table(existing));
            }
            // Build on_notification and on_exit callbacks
            let mk2 = Arc::clone(&mk);
            let on_notification = lua.create_function(move |lua, (client, message): (LuaTable, LuaTable)| {
                let method: String = message.get("method")?;
                if method == "textDocument/publishDiagnostics" {
                    let m: LuaTable = lua.registry_value(&mk2)?;
                    let params: LuaTable = message.get("params").unwrap_or_else(|_| lua.create_table().unwrap());
                    mgr_publish_diagnostics(lua, &m, &client, &params)?;
                }
                Ok(())
            })?;
            let mk3 = Arc::clone(&mk);
            let key2 = key.clone();
            let on_exit = lua.create_function(move |lua, exited_client: LuaTable| {
                let m: LuaTable = lua.registry_value(&mk3)?;
                let clients: LuaTable = m.get("clients")?;
                clients.set(key2.clone(), LuaValue::Nil)?;
                let doc_state: LuaTable = m.get("doc_state")?;
                let mut to_remove: Vec<LuaValue> = Vec::new();
                for pair in doc_state.clone().pairs::<LuaValue, LuaTable>() {
                    let (doc_val, state) = pair?;
                    let client_val: LuaValue = state.get("client")?;
                    if let LuaValue::Table(c) = client_val {
                        let exit_name: String = exited_client.get("name").unwrap_or_default();
                        let c_name: String = c.get("name").unwrap_or_default();
                        if exit_name == c_name { to_remove.push(doc_val); }
                    }
                }
                for dv in to_remove { doc_state.set(dv, LuaValue::Nil)?; }
                Ok(())
            })?;
            let opts = lua.create_table()?;
            opts.set("on_notification", on_notification)?;
            opts.set("on_exit", on_exit)?;
            let client_class: LuaTable = req(lua, "plugins.lsp.client")?;
            let new_fn: LuaFunction = client_class.get("new")?;
            let result: LuaMultiValue = new_fn.call((spec_name.clone(), spec.clone(), root_dir.clone(), opts))?;
            let mut ri = result.into_iter();
            let client = match ri.next() { Some(LuaValue::Table(t)) => t, _ => {
                let err_msg: String = match ri.next() { Some(LuaValue::String(s)) => s.to_str()?.to_owned(), _ => "unknown".to_owned() };
                let core: LuaTable = req(lua, "core")?;
                core.get::<LuaFunction>("warn")?.call::<()>(format!("Failed to start LSP {}: {}", spec_name, err_msg))?;
                return Ok(LuaValue::Nil);
            }};
            // Build capabilities table
            let caps = lua.create_table()?;
            {
                let general = lua.create_table()?;
                let pos_enc = lua.create_table()?;
                pos_enc.raw_set(1, "utf-8")?; pos_enc.raw_set(2, "utf-16")?;
                general.set("positionEncodings", pos_enc)?;
                caps.set("general", general)?;
            }
            {
                let td = lua.create_table()?;
                let sync = lua.create_table()?;
                sync.set("didSave", true)?; sync.set("willSave", false)?; sync.set("willSaveWaitUntil", false)?;
                td.set("synchronization", sync)?;
                let hover = lua.create_table()?;
                let cf = lua.create_table()?; cf.raw_set(1, "plaintext")?; cf.raw_set(2, "markdown")?;
                hover.set("contentFormat", cf)?; td.set("hover", hover)?;
                let def = lua.create_table()?; def.set("dynamicRegistration", false)?; td.set("definition", def)?;
                let rename = lua.create_table()?; rename.set("dynamicRegistration", false)?; rename.set("prepareSupport", false)?; td.set("rename", rename)?;
                let comp = lua.create_table()?; comp.set("dynamicRegistration", false)?;
                let ci = lua.create_table()?; ci.set("snippetSupport", false)?;
                let df = lua.create_table()?; df.raw_set(1, "plaintext")?; df.raw_set(2, "markdown")?;
                ci.set("documentationFormat", df)?; comp.set("completionItem", ci)?; td.set("completion", comp)?;
                let sem = lua.create_table()?; sem.set("dynamicRegistration", false)?;
                let reqs = lua.create_table()?;
                let full = lua.create_table()?; full.set("delta", false)?; reqs.set("full", full)?; reqs.set("range", false)?;
                sem.set("requests", reqs)?;
                let tt = lua.create_table()?;
                for (i, s) in ["namespace","type","class","enum","interface","struct","typeParameter","parameter","variable","property","enumMember","event","function","method","macro","keyword","modifier","comment","string","number","regexp","operator","decorator"].iter().enumerate() {
                    tt.raw_set(i as i64 + 1, *s)?;
                }
                sem.set("tokenTypes", tt)?;
                sem.set("tokenModifiers", lua.create_table()?)?;
                let fmts = lua.create_table()?; fmts.raw_set(1, "relative")?;
                sem.set("formats", fmts)?;
                sem.set("overlappingTokenSupport", false)?;
                sem.set("multilineTokenSupport", true)?;
                td.set("semanticTokens", sem)?;
                caps.set("textDocument", td)?;
            }
            {
                let ws = lua.create_table()?;
                let we = lua.create_table()?; we.set("documentChanges", true)?;
                ws.set("workspaceEdit", we)?; caps.set("workspace", ws)?;
            }
            let init_params = lua.create_table()?;
            init_params.set("processId", LuaValue::Nil)?;
            let client_info = lua.create_table()?;
            client_info.set("name", "lite-anvil")?;
            let version: LuaValue = lua.globals().get("VERSION").unwrap_or(LuaValue::Nil);
            client_info.set("version", version)?;
            init_params.set("clientInfo", client_info)?;
            init_params.set("rootPath", root_dir.clone())?;
            init_params.set("rootUri", mgr_path_to_uri(lua, &root_dir)?)?;
            let wf = lua.create_table()?;
            let wf1 = lua.create_table()?;
            wf1.set("uri", mgr_path_to_uri(lua, &root_dir)?)?;
            wf1.set("name", mgr_basename(&root_dir).to_owned())?;
            wf.raw_set(1, wf1)?;
            init_params.set("workspaceFolders", wf)?;
            let init_opts: LuaValue = spec.get("initializationOptions").unwrap_or(LuaValue::Nil);
            init_params.set("initializationOptions", init_opts)?;
            init_params.set("capabilities", caps)?;
            let settings: LuaValue = spec.get("settings").unwrap_or(LuaValue::Nil);
            let client2 = client.clone();
            let cb = lua.create_function(move |lua, ready_client: LuaTable| {
                if !matches!(settings.clone(), LuaValue::Nil) {
                    let params = lua.create_table()?;
                    params.set("settings", settings.clone())?;
                    ready_client.get::<LuaFunction>("notify")?
                        .call::<()>((ready_client.clone(), "workspace/didChangeConfiguration", params, LuaValue::Nil))?;
                }
                Ok(())
            })?;
            client2.get::<LuaFunction>("initialize")?.call::<()>((client2.clone(), init_params, cb))?;
            clients.set(key, client.clone())?;
            Ok(LuaValue::Table(client))
        })?)?;
    }

    // ── request_semantic_tokens ────────────────────────────────────────────────
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("request_semantic_tokens", lua.create_function(move |lua, doc: LuaTable| {
            let m: LuaTable = lua.registry_value(&mk)?;
            let doc_state: LuaTable = m.get("doc_state")?;
            let state: LuaValue = doc_state.get(doc.clone())?;
            let state = match state { LuaValue::Table(t) => t, _ => return Ok(()) };
            if state.get::<bool>("semantic_request_in_flight").unwrap_or(false) { return Ok(()); }
            let config: LuaTable = req(lua, "core.config")?;
            let plugin_cfg: LuaTable = config.get::<LuaTable>("plugins")?.get("lsp")?;
            if !plugin_cfg.get::<bool>("semantic_highlighting").unwrap_or(true) { return Ok(()); }
            let client: LuaValue = state.get("client")?;
            let client = match client { LuaValue::Table(t) => t, _ => return Ok(()) };
            let caps: LuaValue = client.get("capabilities")?;
            let has_full = match caps {
                LuaValue::Table(c) => {
                    match c.get::<LuaValue>("semanticTokensProvider")? {
                        LuaValue::Table(p) => !matches!(p.get::<LuaValue>("full")?, LuaValue::Nil | LuaValue::Boolean(false)),
                        _ => false,
                    }
                }
                _ => false,
            };
            if !has_full { return Ok(()); }
            state.set("semantic_request_in_flight", true)?;
            let uri: String = state.get("uri")?;
            let mk2 = Arc::clone(&mk);
            let doc2 = doc.clone();
            let cb = lua.create_function(move |lua, (result, err): (LuaValue, LuaValue)| {
                let m: LuaTable = lua.registry_value(&mk2)?;
                let doc_state: LuaTable = m.get("doc_state")?;
                if let Ok(LuaValue::Table(st)) = doc_state.get::<LuaValue>(doc2.clone()) {
                    st.set("semantic_request_in_flight", false)?;
                }
                if !matches!(err, LuaValue::Nil) {
                    let msg = if let LuaValue::Table(e) = &err {
                        e.get::<String>("message").unwrap_or_else(|_| format!("{:?}", err))
                    } else { format!("{:?}", err) };
                    let core: LuaTable = req(lua, "core")?;
                    core.get::<LuaFunction>("warn")?.call::<()>(format!("LSP semantic tokens failed: {}", msg))?;
                    return Ok(());
                }
                if let LuaValue::Table(result_t) = result {
                    let m: LuaTable = lua.registry_value(&mk2)?;
                    let doc_state: LuaTable = m.get("doc_state")?;
                    let client_val: LuaValue = if let Ok(LuaValue::Table(st)) = doc_state.get::<LuaValue>(doc2.clone()) {
                        st.get("client")?
                    } else { LuaValue::Nil };
                    if let LuaValue::Table(client) = client_val {
                        mgr_apply_semantic_tokens(lua, &m, &doc2, &client, &result_t)?;
                    }
                }
                Ok(())
            })?;
            let params = lua.create_table()?;
            let td = lua.create_table()?; td.set("uri", uri)?;
            params.set("textDocument", td)?;
            client.get::<LuaFunction>("request")?.call::<()>((client, "textDocument/semanticTokens/full", params, cb))
        })?)?;
    }

    // ── schedule_semantic_refresh ──────────────────────────────────────────────
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("schedule_semantic_refresh", lua.create_function(move |lua, (doc, delay): (LuaTable, Option<f64>)| {
            let m: LuaTable = lua.registry_value(&mk)?;
            let doc_state: LuaTable = m.get("doc_state")?;
            let state: LuaValue = doc_state.get(doc)?;
            let state = match state { LuaValue::Table(t) => t, _ => return Ok(()) };
            let uri: String = state.get("uri")?;
            let system: LuaTable = lua.globals().get("system")?;
            let now: f64 = system.get::<LuaFunction>("get_time")?.call(())?;
            let native: LuaTable = req(lua, "lsp_manager")?;
            native.get::<LuaFunction>("schedule_semantic")?.call::<()>((uri, now, delay.unwrap_or(0.35)))
        })?)?;
    }

    // ── open_doc ───────────────────────────────────────────────────────────────
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("open_doc", lua.create_function(move |lua, doc: LuaTable| {
            let m: LuaTable = lua.registry_value(&mk)?;
            if doc.get::<bool>("large_file_mode").unwrap_or(false) { return Ok(LuaValue::Nil); }
            let ensure: LuaFunction = m.get("ensure_client")?;
            let client_val: LuaValue = ensure.call(doc.clone())?;
            let client = match client_val { LuaValue::Table(t) => t, _ => return Ok(LuaValue::Nil) };
            let doc_state: LuaTable = m.get("doc_state")?;
            if !matches!(doc_state.get::<LuaValue>(doc.clone())?, LuaValue::Nil) {
                return Ok(LuaValue::Table(client));
            }
            let abs: String = doc.get("abs_filename")?;
            let uri = mgr_path_to_uri(lua, &abs)?;
            let version: LuaValue = doc.get::<LuaFunction>("get_change_id")?.call(doc.clone())?;
            let state = lua.create_table()?;
            state.set("client", client.clone())?;
            state.set("uri", uri.clone())?;
            state.set("version", version.clone())?;
            state.set("semantic_lines", lua.create_table()?)?;
            state.set("last_diagnostic_version", LuaValue::Nil)?;
            doc_state.set(doc.clone(), state)?;
            let native: LuaTable = req(lua, "lsp_manager")?;
            native.get::<LuaFunction>("open_doc")?.call::<()>((uri.clone(), version.clone()))?;
            let syn_name: String = doc.get::<LuaTable>("syntax")
                .and_then(|s| s.get::<String>("name"))
                .map(|n| n.to_lowercase())
                .unwrap_or_else(|_| "plaintext".to_owned());
            let full_text: String = doc.get::<LuaFunction>("get_text")?
                .call::<String>((doc.clone(), 1i64, 1i64, f64::INFINITY, f64::INFINITY))?;
            let td = lua.create_table()?;
            td.set("uri", uri)?;
            td.set("languageId", syn_name)?;
            td.set("version", version)?;
            td.set("text", full_text)?;
            let params = lua.create_table()?; params.set("textDocument", td)?;
            client.get::<LuaFunction>("notify")?
                .call::<()>((client.clone(), "textDocument/didOpen", params, LuaValue::Nil))?;
            Ok(LuaValue::Table(client))
        })?)?;
    }

    // ── on_doc_change ──────────────────────────────────────────────────────────
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("on_doc_change", lua.create_function(move |lua, doc: LuaTable| {
            let m: LuaTable = lua.registry_value(&mk)?;
            let doc_state: LuaTable = m.get("doc_state")?;
            let state_val: LuaValue = doc_state.get(doc.clone())?;
            let state = match state_val {
                LuaValue::Table(t) => t,
                _ => {
                    m.get::<LuaFunction>("open_doc")?.call::<LuaValue>(doc.clone())?;
                    match doc_state.get::<LuaValue>(doc.clone())? {
                        LuaValue::Table(t) => t,
                        _ => return Ok(()),
                    }
                }
            };
            let version: LuaValue = doc.get::<LuaFunction>("get_change_id")?.call(doc.clone())?;
            state.set("version", version.clone())?;
            let uri: String = state.get("uri")?;
            let native: LuaTable = req(lua, "lsp_manager")?;
            native.get::<LuaFunction>("update_doc")?.call::<()>((uri.clone(), version.clone()))?;
            let client: LuaTable = state.get("client")?;
            let td = lua.create_table()?; td.set("uri", uri)?; td.set("version", version)?;
            let change = lua.create_table()?;
            let full_text: String = doc.get::<LuaFunction>("get_text")?
                .call::<String>((doc.clone(), 1i64, 1i64, f64::INFINITY, f64::INFINITY))?;
            let cc = lua.create_table()?; cc.set("text", full_text)?;
            change.raw_set(1, cc)?;
            let params = lua.create_table()?;
            params.set("textDocument", td)?; params.set("contentChanges", change)?;
            client.get::<LuaFunction>("notify")?
                .call::<()>((client, "textDocument/didChange", params, LuaValue::Nil))?;
            m.get::<LuaFunction>("schedule_semantic_refresh")?.call::<()>((doc, 0.35f64))?;
            let core: LuaTable = req(lua, "core")?;
            core.set("redraw", true)
        })?)?;
    }

    // ── on_doc_save ────────────────────────────────────────────────────────────
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("on_doc_save", lua.create_function(move |lua, doc: LuaTable| {
            let m: LuaTable = lua.registry_value(&mk)?;
            let doc_state: LuaTable = m.get("doc_state")?;
            let state: LuaValue = doc_state.get(doc.clone())?;
            let state = match state { LuaValue::Table(t) => t, _ => return Ok(()) };
            let uri: String = state.get("uri")?;
            let client: LuaTable = state.get("client")?;
            let full_text: String = doc.get::<LuaFunction>("get_text")?
                .call::<String>((doc.clone(), 1i64, 1i64, f64::INFINITY, f64::INFINITY))?;
            let td = lua.create_table()?; td.set("uri", uri)?;
            let params = lua.create_table()?;
            params.set("textDocument", td)?; params.set("text", full_text)?;
            client.get::<LuaFunction>("notify")?
                .call::<()>((client, "textDocument/didSave", params, LuaValue::Nil))?;
            m.get::<LuaFunction>("schedule_semantic_refresh")?.call::<()>((doc, 0.1f64))?;
            let core: LuaTable = req(lua, "core")?;
            core.set("redraw", true)
        })?)?;
    }

    // ── on_doc_close ───────────────────────────────────────────────────────────
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("on_doc_close", lua.create_function(move |lua, doc: LuaTable| {
            let m: LuaTable = lua.registry_value(&mk)?;
            let doc_state: LuaTable = m.get("doc_state")?;
            let state: LuaValue = doc_state.get(doc.clone())?;
            let state = match state { LuaValue::Table(t) => t, _ => return Ok(()) };
            let uri: String = state.get("uri")?;
            let client: LuaTable = state.get("client")?;
            let td = lua.create_table()?; td.set("uri", uri.clone())?;
            let params = lua.create_table()?; params.set("textDocument", td)?;
            client.get::<LuaFunction>("notify")?
                .call::<()>((client, "textDocument/didClose", params, LuaValue::Nil))?;
            let native: LuaTable = req(lua, "lsp_manager")?;
            native.get::<LuaFunction>("close_doc")?.call::<()>(uri)?;
            doc_state.set(doc, LuaValue::Nil)?;
            let core: LuaTable = req(lua, "core")?;
            core.set("redraw", true)
        })?)?;
    }

    // ── document_params ────────────────────────────────────────────────────────
    {
        mgr.set("document_params", lua.create_function(|lua, (doc, line, col): (LuaTable, Option<i64>, Option<i64>)| {
            let (line, col) = match (line, col) {
                (Some(l), Some(c)) => (l, c),
                _ => {
                    let sel: LuaMultiValue = doc.get::<LuaFunction>("get_selection")?.call(doc.clone())?;
                    let mut it = sel.into_iter();
                    let l = match it.next() { Some(LuaValue::Integer(n)) => n, _ => 1 };
                    let c = match it.next() { Some(LuaValue::Integer(n)) => n, _ => 1 };
                    (l, c)
                }
            };
            let abs: String = doc.get("abs_filename")?;
            let uri = mgr_path_to_uri(lua, &abs)?;
            let pos = mgr_lsp_pos(lua, &doc, line, col)?;
            let td = lua.create_table()?; td.set("uri", uri)?;
            let params = lua.create_table()?;
            params.set("textDocument", td)?;
            params.set("position", pos)?;
            Ok(params)
        })?)?;
    }

    // ── get_line_diagnostic_severity ───────────────────────────────────────────
    {
        mgr.set("get_line_diagnostic_severity", lua.create_function(|lua, (doc, line): (LuaTable, i64)| {
            let abs: LuaValue = doc.get("abs_filename")?;
            if let LuaValue::String(s) = abs {
                let s = s.to_str()?.to_owned();
                let uri = mgr_path_to_uri(lua, &s)?;
                let native: LuaTable = req(lua, "lsp_manager")?;
                let f: LuaFunction = native.get("get_line_diagnostic_severity")?;
                return f.call::<LuaValue>((uri, line));
            }
            let diagnostics = mgr_get_doc_diagnostics(lua, &doc)?;
            let mut severity: Option<i64> = None;
            for item in diagnostics.sequence_values::<LuaTable>() {
                let d = item?;
                if let Some((sl, el)) = mgr_diagnostic_line_range(&d)? {
                    if line >= sl && line <= el {
                        let s = d.get::<i64>("severity").unwrap_or(3);
                        if severity.is_none_or(|cur| s < cur) { severity = Some(s); }
                    }
                }
            }
            Ok(match severity { Some(s) => LuaValue::Integer(s), None => LuaValue::Nil })
        })?)?;
    }

    // ── get_line_diagnostic_segments ───────────────────────────────────────────
    {
        mgr.set("get_line_diagnostic_segments", lua.create_function(|lua, (doc, line): (LuaTable, i64)| {
            let diagnostics = mgr_get_doc_diagnostics(lua, &doc)?;
            if diagnostics.raw_len() == 0 { return Ok(LuaValue::Nil); }
            let lines_t: LuaTable = doc.get("lines")?;
            let line_text: String = lines_t.get::<String>(line).unwrap_or_else(|_| "\n".to_owned());
            let max_col = (line_text.len() as i64).max(1);
            let common: LuaTable = req(lua, "core.common")?;
            let clamp: LuaFunction = common.get("clamp")?;
            let segments = lua.create_table()?;
            for item in diagnostics.sequence_values::<LuaTable>() {
                let d = item?;
                let range: LuaValue = d.get("range")?;
                let range = match range { LuaValue::Table(t) => t, _ => continue };
                let (sl, el) = match mgr_diagnostic_line_range(&d)? { Some(p) => p, None => continue };
                if line < sl || line > el { continue; }
                let start: LuaTable = range.get("start")?;
                let end_t: LuaTable = range.get::<LuaTable>("end").or_else(|_| range.get("start"))?;
                let col1 = if line == sl { mgr_doc_pos_from_lsp(&doc, &start)?.1 } else { 1i64 };
                let col2 = if line == el { mgr_doc_pos_from_lsp(&doc, &end_t)?.1 } else { max_col };
                let col1: i64 = clamp.call((col1, 1i64, max_col))?;
                let col2: i64 = clamp.call((col2.max(col1 + 1), 1i64, max_col))?;
                let seg = lua.create_table()?;
                seg.set("col1", col1)?; seg.set("col2", col2)?;
                seg.set("severity", d.get::<i64>("severity").unwrap_or(3))?;
                segments.raw_set(segments.raw_len() + 1, seg)?;
            }
            if segments.raw_len() == 0 { return Ok(LuaValue::Nil); }
            let table_lib: LuaTable = lua.globals().get("table")?;
            let sort: LuaFunction = table_lib.get("sort")?;
            sort.call::<()>((segments.clone(), lua.create_function(|_lua, (a, b): (LuaTable, LuaTable)| {
                let ac: i64 = a.get("col1")?; let bc: i64 = b.get("col1")?;
                if ac == bc { return Ok(a.get::<i64>("col2")? < b.get::<i64>("col2")?); }
                Ok(ac < bc)
            })?))?;
            Ok(LuaValue::Table(segments))
        })?)?;
    }

    // ── get_hover_diagnostic ───────────────────────────────────────────────────
    {
        mgr.set("get_hover_diagnostic", lua.create_function(|lua, (doc, line, col): (LuaTable, i64, LuaValue)| {
            let diagnostics = mgr_get_doc_diagnostics(lua, &doc)?;
            if diagnostics.raw_len() == 0 { return Ok(LuaValue::Nil); }
            let col_opt: Option<i64> = match col { LuaValue::Integer(n) => Some(n), _ => None };
            let mut best: Option<LuaTable> = None;
            for item in diagnostics.sequence_values::<LuaTable>() {
                let d = item?;
                let range: LuaValue = d.get("range")?;
                let range = match range { LuaValue::Table(t) => t, _ => continue };
                let (sl, el) = match mgr_diagnostic_line_range(&d)? { Some(p) => p, None => continue };
                if line < sl || line > el { continue; }
                let start: LuaTable = range.get("start")?;
                let end_t: LuaTable = range.get::<LuaTable>("end").or_else(|_| range.get("start"))?;
                let start_col = if line == sl { mgr_doc_pos_from_lsp(&doc, &start)?.1 } else { 1i64 };
                let end_col   = if line == el { mgr_doc_pos_from_lsp(&doc, &end_t)?.1 } else { i64::MAX };
                let within = match col_opt {
                    None => true,
                    Some(c) => c >= start_col && c <= start_col.max(start_col + 1).max(end_col),
                };
                if within {
                    let sev = d.get::<i64>("severity").unwrap_or(3);
                    let msg_len = d.get::<String>("message").map(|s| s.len()).unwrap_or(0);
                    let replace = match &best {
                        None => true,
                        Some(b) => {
                            let bs = b.get::<i64>("severity").unwrap_or(3);
                            let bm = b.get::<String>("message").map(|s| s.len()).unwrap_or(0);
                            sev < bs || (sev == bs && msg_len > bm)
                        }
                    };
                    if replace { best = Some(d); }
                }
            }
            Ok(match best { Some(t) => LuaValue::Table(t), None => LuaValue::Nil })
        })?)?;
    }

    // ── get_inline_diagnostic ──────────────────────────────────────────────────
    {
        mgr.set("get_inline_diagnostic", lua.create_function(|lua, (doc, line): (LuaTable, i64)| {
            let diagnostics = mgr_get_doc_diagnostics(lua, &doc)?;
            if diagnostics.raw_len() == 0 { return Ok(LuaMultiValue::new()); }
            let mut best: Option<LuaTable> = None;
            let mut best_end_col: Option<i64> = None;
            for item in diagnostics.sequence_values::<LuaTable>() {
                let d = item?;
                let (sl, el) = match mgr_diagnostic_line_range(&d)? { Some(p) => p, None => continue };
                if sl != line { continue; }
                let range: LuaTable = match d.get::<LuaValue>("range")? { LuaValue::Table(t) => t, _ => continue };
                let sev = d.get::<i64>("severity").unwrap_or(3);
                let end_t: LuaTable = range.get::<LuaTable>("end").or_else(|_| range.get("start"))?;
                let end_col = if line == el { mgr_doc_pos_from_lsp(&doc, &end_t)?.1 } else {
                    let start: LuaTable = range.get("start")?;
                    mgr_doc_pos_from_lsp(&doc, &start)?.1
                };
                let msg_len = d.get::<String>("message").map(|s| s.len()).unwrap_or(0);
                let replace = match &best {
                    None => true,
                    Some(b) => {
                        let bs = b.get::<i64>("severity").unwrap_or(3);
                        let bm = b.get::<String>("message").map(|s| s.len()).unwrap_or(0);
                        sev < bs || (sev == bs && msg_len > bm)
                    }
                };
                if replace { best = Some(d); best_end_col = Some(end_col); }
            }
            match best {
                None => Ok(LuaMultiValue::new()),
                Some(t) => Ok(LuaMultiValue::from_vec(vec![
                    LuaValue::Table(t),
                    best_end_col.map(LuaValue::Integer).unwrap_or(LuaValue::Nil),
                ])),
            }
        })?)?;
    }

    // ── goto_definition ────────────────────────────────────────────────────────
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("goto_definition", lua.create_function(move |lua, ()| {
            let m: LuaTable = lua.registry_value(&mk)?;
            let view = match mgr_current_docview(lua)? { Some(v) => v, None => return Ok(()) };
            let doc: LuaTable = view.get("doc")?;
            if matches!(doc.get::<LuaValue>("abs_filename")?, LuaValue::Nil) { return Ok(()); }
            let client = match mgr_navigation_client(lua, &m, &doc, Some("definitionProvider"), "goto definition")? {
                Some(c) => c, None => return Ok(()),
            };
            let origin = mgr_capture_view_location(lua, &view)?;
            let params: LuaTable = m.get::<LuaFunction>("document_params")?.call(doc)?;
            let mk2 = Arc::clone(&mk);
            let cb = lua.create_function(move |lua, (result, err): (LuaValue, LuaValue)| {
                let m: LuaTable = lua.registry_value(&mk2)?;
                if !matches!(err, LuaValue::Nil) {
                    let msg = if let LuaValue::Table(e) = &err { e.get::<String>("message").unwrap_or_default() } else { format!("{:?}", err) };
                    req(lua, "core")?.get::<LuaFunction>("warn")?.call::<()>(format!("LSP definition failed: {}", msg))?;
                    return Ok(());
                }
                let locs = match result {
                    LuaValue::Table(t) => {
                        if matches!(t.get::<LuaValue>(1)?, LuaValue::Table(_)) { t }
                        else { let r = lua.create_table()?; r.raw_set(1, t)?; r }
                    }
                    _ => { req(lua, "core")?.get::<LuaFunction>("warn")?.call::<()>("LSP definition returned no result")?; return Ok(()); }
                };
                let items = mgr_make_location_items(lua, &locs)?;
                if items.raw_len() == 1 {
                    let item: LuaTable = items.get(1)?;
                    let payload: LuaTable = item.get("payload")?;
                    let uri: String = payload.get("uri")?;
                    let range: LuaTable = payload.get("range")?;
                    mgr_open_location(lua, &m, &uri, &range, origin.clone())?;
                } else {
                    let origin2 = origin.clone();
                    let mk3 = Arc::clone(&mk2);
                    mgr_pick_from_list(lua, "Definitions", items, lua.create_function(move |lua, item: LuaTable| {
                        let m: LuaTable = lua.registry_value(&mk3)?;
                        let uri: String = item.get("uri")?;
                        let range: LuaTable = item.get("range")?;
                        mgr_open_location(lua, &m, &uri, &range, origin2.clone())
                    })?)?;
                }
                Ok(())
            })?;
            client.get::<LuaFunction>("request")?.call::<()>((client, "textDocument/definition", params, cb))
        })?)?;
    }

    // ── goto_type_definition / goto_implementation / find_references ───────────
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("goto_type_definition", lua.create_function(move |lua, ()| {
            let m: LuaTable = lua.registry_value(&mk)?;
            let view = match mgr_current_docview(lua)? { Some(v) => v, None => return Ok(()) };
            let doc: LuaTable = view.get("doc")?;
            if matches!(doc.get::<LuaValue>("abs_filename")?, LuaValue::Nil) { return Ok(()); }
            let client = match mgr_navigation_client(lua, &m, &doc, Some("typeDefinitionProvider"), "goto type definition")? { Some(c) => c, None => return Ok(()) };
            let origin = mgr_capture_view_location(lua, &view)?;
            let params: LuaTable = m.get::<LuaFunction>("document_params")?.call(doc)?;
            let mk2 = Arc::clone(&mk);
            let cb = lua.create_function(move |lua, (result, err): (LuaValue, LuaValue)| {
                let m: LuaTable = lua.registry_value(&mk2)?;
                if !matches!(err, LuaValue::Nil) { return Ok(()); }
                let locs = match result { LuaValue::Table(t) => { if matches!(t.get::<LuaValue>(1)?, LuaValue::Table(_)) { t } else { let r = lua.create_table()?; r.raw_set(1, t)?; r } } _ => return Ok(()) };
                let items = mgr_make_location_items(lua, &locs)?;
                if items.raw_len() == 1 {
                    let item: LuaTable = items.get(1)?; let payload: LuaTable = item.get("payload")?;
                    let uri: String = payload.get("uri")?; let range: LuaTable = payload.get("range")?;
                    mgr_open_location(lua, &m, &uri, &range, origin.clone())?;
                } else {
                    let o2 = origin.clone(); let mk3 = Arc::clone(&mk2);
                    mgr_pick_from_list(lua, "Type Definitions", items, lua.create_function(move |lua, item: LuaTable| {
                        let m: LuaTable = lua.registry_value(&mk3)?;
                        let uri: String = item.get("uri")?; let range: LuaTable = item.get("range")?;
                        mgr_open_location(lua, &m, &uri, &range, o2.clone())
                    })?)?;
                }
                Ok(())
            })?;
            client.get::<LuaFunction>("request")?.call::<()>((client, "textDocument/typeDefinition", params, cb))
        })?)?;
    }
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("goto_implementation", lua.create_function(move |lua, ()| {
            let m: LuaTable = lua.registry_value(&mk)?;
            let view = match mgr_current_docview(lua)? { Some(v) => v, None => return Ok(()) };
            let doc: LuaTable = view.get("doc")?;
            if matches!(doc.get::<LuaValue>("abs_filename")?, LuaValue::Nil) { return Ok(()); }
            let client = match mgr_navigation_client(lua, &m, &doc, Some("implementationProvider"), "goto implementation")? { Some(c) => c, None => return Ok(()) };
            let origin = mgr_capture_view_location(lua, &view)?;
            let params: LuaTable = m.get::<LuaFunction>("document_params")?.call(doc)?;
            let mk2 = Arc::clone(&mk);
            let cb = lua.create_function(move |lua, (result, err): (LuaValue, LuaValue)| {
                let m: LuaTable = lua.registry_value(&mk2)?;
                if !matches!(err, LuaValue::Nil) { return Ok(()); }
                let locs = match result { LuaValue::Table(t) => { if matches!(t.get::<LuaValue>(1)?, LuaValue::Table(_)) { t } else { let r = lua.create_table()?; r.raw_set(1, t)?; r } } _ => return Ok(()) };
                let items = mgr_make_location_items(lua, &locs)?;
                if items.raw_len() == 1 {
                    let item: LuaTable = items.get(1)?; let payload: LuaTable = item.get("payload")?;
                    let uri: String = payload.get("uri")?; let range: LuaTable = payload.get("range")?;
                    mgr_open_location(lua, &m, &uri, &range, origin.clone())?;
                } else {
                    let o2 = origin.clone(); let mk3 = Arc::clone(&mk2);
                    mgr_pick_from_list(lua, "Implementation", items, lua.create_function(move |lua, item: LuaTable| {
                        let m: LuaTable = lua.registry_value(&mk3)?;
                        let uri: String = item.get("uri")?; let range: LuaTable = item.get("range")?;
                        mgr_open_location(lua, &m, &uri, &range, o2.clone())
                    })?)?;
                }
                Ok(())
            })?;
            client.get::<LuaFunction>("request")?.call::<()>((client, "textDocument/implementation", params, cb))
        })?)?;
    }
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("find_references", lua.create_function(move |lua, ()| {
            let m: LuaTable = lua.registry_value(&mk)?;
            let view = match mgr_current_docview(lua)? { Some(v) => v, None => return Ok(()) };
            let doc: LuaTable = view.get("doc")?;
            if matches!(doc.get::<LuaValue>("abs_filename")?, LuaValue::Nil) { return Ok(()); }
            let client = match mgr_navigation_client(lua, &m, &doc, Some("referencesProvider"), "find references")? { Some(c) => c, None => return Ok(()) };
            let origin = mgr_capture_view_location(lua, &view)?;
            let params: LuaTable = m.get::<LuaFunction>("document_params")?.call(doc)?;
            let ctx = lua.create_table()?; ctx.set("includeDeclaration", true)?;
            params.set("context", ctx)?;
            let mk2 = Arc::clone(&mk);
            let cb = lua.create_function(move |lua, (result, err): (LuaValue, LuaValue)| {
                if !matches!(err, LuaValue::Nil) { return Ok(()); }
                let locs = match result { LuaValue::Table(t) => t, _ => lua.create_table()? };
                let items = mgr_make_location_items(lua, &locs)?;
                let o2 = origin.clone(); let mk3 = Arc::clone(&mk2);
                mgr_pick_from_list(lua, "References", items, lua.create_function(move |lua, item: LuaTable| {
                    let m: LuaTable = lua.registry_value(&mk3)?;
                    let uri: String = item.get("uri")?; let range: LuaTable = item.get("range")?;
                    mgr_open_location(lua, &m, &uri, &range, o2.clone())
                })?)
            })?;
            client.get::<LuaFunction>("request")?.call::<()>((client, "textDocument/references", params, cb))
        })?)?;
    }

    // ── jump_back ──────────────────────────────────────────────────────────────
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("jump_back", lua.create_function(move |lua, ()| {
            let m: LuaTable = lua.registry_value(&mk)?;
            let current = mgr_current_docview(lua)?.and_then(|v| mgr_capture_view_location(lua, &v).ok().flatten());
            let loc_history: LuaTable = m.get("location_history")?;
            let table_lib: LuaTable = lua.globals().get("table")?;
            let remove: LuaFunction = table_lib.get("remove")?;
            loop {
                let len = loc_history.raw_len();
                if len == 0 {
                    let core: LuaTable = req(lua, "core")?;
                    core.get::<LuaFunction>("warn")?.call::<()>("LSP jump history is empty")?;
                    return Ok(false);
                }
                let location: LuaTable = remove.call((loc_history.clone(), LuaValue::Nil))?;
                let is_same = current.as_ref().map(|c| {
                    let cp: String = c.get("path").unwrap_or_default();
                    let cl: i64    = c.get("line1").unwrap_or(0);
                    let cc: i64    = c.get("col1").unwrap_or(0);
                    let lp: String = location.get("path").unwrap_or_default();
                    let ll: i64    = location.get("line1").unwrap_or(0);
                    let lc: i64    = location.get("col1").unwrap_or(0);
                    cp == lp && cl == ll && cc == lc
                }).unwrap_or(false);
                if !is_same { return mgr_open_captured_location(lua, &location); }
            }
        })?)?;
    }

    // ── hover ──────────────────────────────────────────────────────────────────
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("hover", lua.create_function(move |lua, ()| {
            let m: LuaTable = lua.registry_value(&mk)?;
            let view = match mgr_current_docview(lua)? { Some(v) => v, None => return Ok(()) };
            let doc: LuaTable = view.get("doc")?;
            if matches!(doc.get::<LuaValue>("abs_filename")?, LuaValue::Nil) { return Ok(()); }
            let client_val: LuaValue = m.get::<LuaFunction>("open_doc")?.call(doc.clone())?;
            let client = match client_val { LuaValue::Table(t) => t, _ => {
                let core: LuaTable = req(lua, "core")?;
                let name: String = doc.get::<LuaFunction>("get_name")?.call(doc)?;
                core.get::<LuaFunction>("warn")?.call::<()>(format!("No LSP server configured for {}", name))?;
                return Ok(());
            }};
            let params: LuaTable = m.get::<LuaFunction>("document_params")?.call(doc)?;
            let cb = lua.create_function(|lua, (result, err): (LuaValue, LuaValue)| {
                if !matches!(err, LuaValue::Nil) { return Ok(()); }
                let text = match result {
                    LuaValue::Table(t) => {
                        let contents: LuaValue = t.get("contents")?;
                        mgr_content_to_text(&contents)?
                    }
                    _ => String::new(),
                };
                if text.is_empty() { return Ok(()); }
                let core: LuaTable = req(lua, "core")?;
                let sv: LuaTable = core.get("status_view")?;
                let style: LuaTable = req(lua, "core.style")?;
                let text_color: LuaValue = style.get("text")?;
                let display: String = text.replace(|c: char| c.is_whitespace(), " ");
                let display = &display[..display.len().min(240)];
                sv.get::<LuaFunction>("show_message")?.call::<()>((sv, "i", text_color, display.to_owned()))
            })?;
            client.get::<LuaFunction>("request")?.call::<()>((client, "textDocument/hover", params, cb))
        })?)?;
    }

    // ── show_diagnostics ───────────────────────────────────────────────────────
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("show_diagnostics", lua.create_function(move |lua, ()| {
            let view = match mgr_current_docview(lua)? { Some(v) => v, None => return Ok(()) };
            let doc: LuaTable = view.get("doc")?;
            if matches!(doc.get::<LuaValue>("abs_filename")?, LuaValue::Nil) { return Ok(()); }
            let abs: String = doc.get("abs_filename")?;
            let uri = mgr_path_to_uri(lua, &abs)?;
            let diagnostics = mgr_get_sorted_doc_diagnostics(lua, &doc)?;
            if diagnostics.raw_len() == 0 {
                let core: LuaTable = req(lua, "core")?;
                let name: String = doc.get::<LuaFunction>("get_name")?.call(doc)?;
                core.get::<LuaFunction>("log")?.call::<()>(format!("No LSP diagnostics for {}", name))?;
                return Ok(());
            }
            let items = lua.create_table()?;
            let len = diagnostics.raw_len();
            for i in 1..=len {
                let d: LuaTable = diagnostics.get(i)?;
                let range: LuaValue = d.get("range")?;
                if let LuaValue::Table(r) = range {
                    let start: LuaTable = r.get("start")?;
                    let line_no = start.get::<i64>("line").unwrap_or(0) + 1;
                    let char_no = start.get::<i64>("character").unwrap_or(0) + 1;
                    let msg: String = d.get::<String>("message").unwrap_or_default();
                    let msg_short: String = msg.split_whitespace().collect::<Vec<_>>().join(" ");
                    let msg_short = &msg_short[..msg_short.len().min(100)];
                    let text = format!("{:03} L{}:{} {}", i, line_no, char_no, msg_short);
                    let info: String = d.get::<String>("source")
                        .or_else(|_| d.get::<i64>("code").map(|c| c.to_string()))
                        .unwrap_or_default();
                    let range_copy = lua.create_table()?;
                    let rng_t = lua.create_table()?;
                    for pair in r.clone().pairs::<LuaValue, LuaValue>() {
                        let (k, v) = pair?; rng_t.set(k, v)?;
                    }
                    let payload = lua.create_table()?;
                    payload.set("uri", uri.clone())?; payload.set("range", rng_t)?;
                    let item = lua.create_table()?;
                    item.set("text", text)?; item.set("info", info)?; item.set("payload", payload)?;
                    items.raw_set(items.raw_len() + 1, item)?;
                    let _ = range_copy;
                }
            }
            let mk2 = Arc::clone(&mk);
            mgr_pick_from_list(lua, "Diagnostics", items, lua.create_function(move |lua, item: LuaTable| {
                let m: LuaTable = lua.registry_value(&mk2)?;
                let uri: String = item.get("uri")?;
                let range: LuaTable = item.get("range")?;
                mgr_open_location(lua, &m, &uri, &range, None)
            })?)
        })?)?;
    }

    // ── show_document_symbols ──────────────────────────────────────────────────
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("show_document_symbols", lua.create_function(move |lua, ()| {
            let m: LuaTable = lua.registry_value(&mk)?;
            let view = match mgr_current_docview(lua)? { Some(v) => v, None => return Ok(()) };
            let doc: LuaTable = view.get("doc")?;
            if matches!(doc.get::<LuaValue>("abs_filename")?, LuaValue::Nil) { return Ok(()); }
            let client = match mgr_navigation_client(lua, &m, &doc, Some("documentSymbolProvider"), "show document symbols")? { Some(c) => c, None => return Ok(()) };
            let origin = mgr_capture_view_location(lua, &view)?;
            let abs: String = doc.get("abs_filename")?;
            let uri = mgr_path_to_uri(lua, &abs)?;
            let td = lua.create_table()?; td.set("uri", uri.clone())?;
            let params = lua.create_table()?; params.set("textDocument", td)?;
            let mk2 = Arc::clone(&mk);
            let cb = lua.create_function(move |lua, (result, err): (LuaValue, LuaValue)| {
                if !matches!(err, LuaValue::Nil) { return Ok(()); }
                let symbols = match result { LuaValue::Table(t) => t, _ => lua.create_table()? };
                let out = lua.create_table()?;
                mgr_flatten_document_symbols(lua, &symbols, &uri, &out, "")?;
                let o2 = origin.clone(); let mk3 = Arc::clone(&mk2);
                mgr_pick_from_list(lua, "Document Symbols", out, lua.create_function(move |lua, item: LuaTable| {
                    let m: LuaTable = lua.registry_value(&mk3)?;
                    let uri: String = item.get("uri")?; let range: LuaTable = item.get("range")?;
                    mgr_open_location(lua, &m, &uri, &range, o2.clone())
                })?)
            })?;
            client.get::<LuaFunction>("request")?.call::<()>((client, "textDocument/documentSymbol", params, cb))
        })?)?;
    }

    // ── request_code_actions ───────────────────────────────────────────────────
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("request_code_actions", lua.create_function(move |lua, options: LuaValue| {
            let m: LuaTable = lua.registry_value(&mk)?;
            let opts = match options { LuaValue::Table(t) => t, _ => lua.create_table()? };
            let view = match mgr_current_docview(lua)? { Some(v) => v, None => return Ok(()) };
            let doc: LuaTable = view.get("doc")?;
            if matches!(doc.get::<LuaValue>("abs_filename")?, LuaValue::Nil) { return Ok(()); }
            let only_val: LuaValue = opts.get("only").unwrap_or(LuaValue::Nil);
            let action_label = if !matches!(only_val, LuaValue::Nil) { "quick fixes" } else { "code actions" };
            let client = match mgr_navigation_client(lua, &m, &doc, Some("codeActionProvider"), action_label)? { Some(c) => c, None => return Ok(()) };
            let (line1, col1, line2, col2) = if let Ok(LuaValue::Integer(line)) = opts.get::<LuaValue>("line") {
                let col1: i64 = opts.get::<i64>("col1").unwrap_or(1);
                let line2: i64 = opts.get::<i64>("line2").unwrap_or(line);
                let line_text: String = doc.get::<LuaTable>("lines")?.get::<String>(line2).unwrap_or_else(|_| "\n".to_owned());
                let col2: i64 = opts.get::<i64>("col2").unwrap_or_else(|_| (line_text.len() as i64).max(1));
                (line, col1, line2, col2)
            } else {
                let sel: LuaMultiValue = doc.get::<LuaFunction>("get_selection")?.call((doc.clone(), true))?;
                let mut it = sel.into_iter();
                let l1 = match it.next() { Some(LuaValue::Integer(n)) => n, _ => 1 };
                let c1 = match it.next() { Some(LuaValue::Integer(n)) => n, _ => 1 };
                let l2 = match it.next() { Some(LuaValue::Integer(n)) => n, _ => l1 };
                let c2 = match it.next() { Some(LuaValue::Integer(n)) => n, _ => c1 };
                (l1, c1, l2, c2)
            };
            let abs: String = doc.get("abs_filename")?;
            let uri = mgr_path_to_uri(lua, &abs)?;
            // Build diagnostics context
            let all_diag = mgr_get_doc_diagnostics(lua, &doc)?;
            let ctx_diags = lua.create_table()?;
            for item in all_diag.sequence_values::<LuaTable>() {
                let d = item?;
                let range: LuaValue = d.get("range")?;
                if let LuaValue::Table(r) = &range {
                    let start: LuaTable = r.get("start")?;
                    let end_t: LuaTable = r.get::<LuaTable>("end").or_else(|_| r.get("start"))?;
                    let (sl, sc) = mgr_doc_pos_from_lsp(&doc, &start)?;
                    let (el, ec) = mgr_doc_pos_from_lsp(&doc, &end_t)?;
                    let intersects = (sl < line2 || (sl == line2 && sc <= col2)) && (el > line1 || (el == line1 && ec >= col1));
                    if intersects { ctx_diags.raw_set(ctx_diags.raw_len() + 1, d)?; }
                }
            }
            let context = lua.create_table()?;
            context.set("diagnostics", ctx_diags)?;
            if !matches!(only_val, LuaValue::Nil) { context.set("only", only_val)?; }
            let range_t = lua.create_table()?;
            let start_t = mgr_lsp_pos(lua, &doc, line1, col1)?;
            let end_t   = mgr_lsp_pos(lua, &doc, line2, col2)?;
            range_t.set("start", start_t)?; range_t.set("end", end_t)?;
            let td = lua.create_table()?; td.set("uri", uri)?;
            let params = lua.create_table()?;
            params.set("textDocument", td)?; params.set("range", range_t)?; params.set("context", context)?;
            let auto_apply: bool = opts.get("auto_apply_single").unwrap_or(false);
            let label: String = opts.get::<String>("label").unwrap_or_else(|_| "Code Actions".to_owned());
            let client_for_cb = client.clone();
            let cb = lua.create_function(move |lua, (result, err): (LuaValue, LuaValue)| {
                if !matches!(err, LuaValue::Nil) { return Ok(()); }
                let result_list = match result { LuaValue::Table(t) => t, _ => lua.create_table()? };
                // Collect non-disabled actions
                let actions = lua.create_table()?;
                for item in result_list.sequence_values::<LuaTable>() {
                    let action = item?;
                    let disabled: LuaValue = action.get("disabled").unwrap_or(LuaValue::Nil);
                    if matches!(disabled, LuaValue::Nil) { actions.raw_set(actions.raw_len() + 1, action)?; }
                }
                // Sort actions
                let table_lib: LuaTable = lua.globals().get("table")?;
                table_lib.get::<LuaFunction>("sort")?.call::<()>((actions.clone(), lua.create_function(|_lua, (a, b): (LuaTable, LuaTable)| {
                    let score = |t: &LuaTable| -> i64 {
                        let mut s: i64 = 0;
                        if t.get::<bool>("isPreferred").unwrap_or(false) { s += 1000; }
                        let kind: String = t.get("kind").unwrap_or_default();
                        if kind == "quickfix" { s += 100; }
                        else if kind.starts_with("quickfix.") { s += 80; }
                        else if kind.starts_with("source.fixAll") { s += 60; }
                        if !matches!(t.get::<LuaValue>("disabled"), Ok(LuaValue::Nil)) { s -= 500; }
                        s
                    };
                    let as_ = score(&a); let bs = score(&b);
                    if as_ == bs { return Ok(a.get::<String>("title").unwrap_or_default() < b.get::<String>("title").unwrap_or_default()); }
                    Ok(as_ > bs)
                })?))?;
                let count = actions.raw_len();
                if count == 0 {
                    let core: LuaTable = req(lua, "core")?;
                    core.get::<LuaFunction>("warn")?.call::<()>(format!("{}: no results", label))?;
                    return Ok(());
                }
                if auto_apply && count == 1 {
                    let action: LuaTable = actions.get(1)?;
                    apply_code_action_inner(lua, &client_for_cb, &action)?;
                    return Ok(());
                }
                // Build picker items
                let items = lua.create_table()?;
                for i in 1..=count {
                    let action: LuaTable = actions.get(i)?;
                    let kind: String = action.get("kind").unwrap_or_default();
                    let tail = kind.rsplit('.').next().unwrap_or(&kind);
                    let mut info = if tail.is_empty() { String::new() } else {
                        let mut t = tail.to_owned();
                        if let Some(first) = t.get_mut(0..1) { first.make_ascii_uppercase(); }
                        t
                    };
                    if action.get::<bool>("isPreferred").unwrap_or(false) {
                        info = if info.is_empty() { "preferred".to_owned() } else { format!("{} · preferred", info) };
                    }
                    let title: String = action.get("title").unwrap_or_else(|_| "Code Action".to_owned());
                    let item = lua.create_table()?;
                    item.set("text", format!("{:03} {}", i, title))?;
                    item.set("info", info)?;
                    item.set("payload", action)?;
                    items.raw_set(i, item)?;
                }
                let client_c = client_for_cb.clone();
                mgr_pick_from_list(lua, &label, items, lua.create_function(move |lua, action: LuaTable| {
                    apply_code_action_inner(lua, &client_c, &action)
                })?)
            })?;
            client.get::<LuaFunction>("request")?.call::<()>((client.clone(), "textDocument/codeAction", params, cb))
        })?)?;
    }

    // ── code_action / quick_fix / quick_fix_for_line ──────────────────────────
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("code_action", lua.create_function(move |lua, ()| {
            let m: LuaTable = lua.registry_value(&mk)?;
            m.get::<LuaFunction>("request_code_actions")?.call::<()>(LuaValue::Nil)
        })?)?;
    }
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("quick_fix", lua.create_function(move |lua, ()| {
            let m: LuaTable = lua.registry_value(&mk)?;
            let opts = lua.create_table()?;
            let only = lua.create_table()?; only.raw_set(1, "quickfix")?;
            opts.set("only", only)?; opts.set("auto_apply_single", true)?; opts.set("label", "Quick Fixes")?;
            m.get::<LuaFunction>("request_code_actions")?.call::<()>(opts)
        })?)?;
    }
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("quick_fix_for_line", lua.create_function(move |lua, line: i64| {
            let m: LuaTable = lua.registry_value(&mk)?;
            let opts = lua.create_table()?;
            let only = lua.create_table()?; only.raw_set(1, "quickfix")?;
            opts.set("only", only)?; opts.set("auto_apply_single", true)?;
            opts.set("label", "Quick Fixes")?; opts.set("line", line)?;
            m.get::<LuaFunction>("request_code_actions")?.call::<()>(opts)
        })?)?;
    }

    // ── signature_help / maybe_trigger_* ──────────────────────────────────────
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("signature_help", lua.create_function(move |lua, ()| {
            let m: LuaTable = lua.registry_value(&mk)?;
            let view = match mgr_current_docview(lua)? { Some(v) => v, None => return Ok(()) };
            let doc: LuaTable = view.get("doc")?;
            if matches!(doc.get::<LuaValue>("abs_filename")?, LuaValue::Nil) { return Ok(()); }
            let client_val: LuaValue = m.get::<LuaFunction>("open_doc")?.call(doc.clone())?;
            let client = match client_val { LuaValue::Table(t) => t, _ => {
                let core: LuaTable = req(lua, "core")?;
                let name: String = doc.get::<LuaFunction>("get_name")?.call(doc)?;
                return core.get::<LuaFunction>("warn")?.call::<()>(format!("No LSP server configured for {}", name));
            }};
            let params: LuaTable = m.get::<LuaFunction>("document_params")?.call(doc)?;
            let cb = lua.create_function(|lua, (result, err): (LuaValue, LuaValue)| {
                if !matches!(err, LuaValue::Nil) { return Ok(()); }
                let result = match result { LuaValue::Table(t) => t, _ => return Ok(()) };
                let sigs: LuaTable = match result.get::<LuaValue>("signatures")? { LuaValue::Table(t) => t, _ => return Ok(()) };
                if sigs.raw_len() == 0 { return Ok(()); }
                let active_idx = result.get::<i64>("activeSignature").unwrap_or(0) + 1;
                let sig: LuaTable = sigs.get::<LuaValue>(active_idx)
                    .ok().and_then(|v| if let LuaValue::Table(t) = v { Some(t) } else { None })
                    .or_else(|| sigs.get::<LuaTable>(1).ok())
                    .ok_or_else(|| mlua::Error::runtime("no signature"))?;
                let mut label: String = sig.get("label").unwrap_or_default();
                let active_param = result.get::<i64>("activeParameter").unwrap_or(0) as usize;
                if let Ok(LuaValue::Table(params)) = sig.get::<LuaValue>("parameters") {
                    if let Ok(LuaValue::Table(param)) = params.get::<LuaValue>(active_param + 1) {
                        if let Ok(LuaValue::String(param_label)) = param.get::<LuaValue>("label") {
                            let pl = param_label.to_str()?.to_owned();
                            label = label.replacen(&pl, &format!("[{}]", pl), 1);
                        }
                    }
                }
                let core: LuaTable = req(lua, "core")?;
                let sv: LuaTable = core.get("status_view")?;
                let style: LuaTable = req(lua, "core.style")?;
                let text_color: LuaValue = style.get("text")?;
                let display = &label[..label.len().min(240)];
                sv.get::<LuaFunction>("show_message")?.call::<()>((sv, "i", text_color, display.to_owned()))
            })?;
            client.get::<LuaFunction>("request")?.call::<()>((client, "textDocument/signatureHelp", params, cb))
        })?)?;
    }
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("maybe_trigger_signature_help", lua.create_function(move |lua, text: String| {
            if text != "(" && text != "," { return Ok(()); }
            let m: LuaTable = lua.registry_value(&mk)?;
            let view = match mgr_current_docview(lua)? { Some(v) => v, None => return Ok(()) };
            let doc: LuaTable = view.get("doc")?;
            if matches!(doc.get::<LuaValue>("abs_filename")?, LuaValue::Nil) { return Ok(()); }
            let doc_state: LuaTable = m.get("doc_state")?;
            let state: LuaValue = doc_state.get(doc)?;
            let state = match state { LuaValue::Table(t) => t, _ => return Ok(()) };
            let client: LuaTable = match state.get::<LuaValue>("client")? { LuaValue::Table(t) => t, _ => return Ok(()) };
            let caps: LuaValue = client.get("capabilities")?;
            let provider = match caps { LuaValue::Table(c) => match c.get::<LuaValue>("signatureHelpProvider")? { LuaValue::Table(t) => t, _ => return Ok(()) }, _ => return Ok(()) };
            let triggers: LuaTable = provider.get::<LuaTable>("triggerCharacters").unwrap_or_else(|_| lua.create_table().unwrap());
            let matched = if triggers.raw_len() == 0 { true } else {
                triggers.sequence_values::<String>().any(|r| r.map(|s| s == text).unwrap_or(false))
            };
            if matched { m.get::<LuaFunction>("signature_help")?.call::<()>(())? }
            Ok(())
        })?)?;
    }
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("maybe_trigger_completion", lua.create_function(move |lua, text: String| {
            let m: LuaTable = lua.registry_value(&mk)?;
            let view = match mgr_current_docview(lua)? { Some(v) => v, None => return Ok(()) };
            let doc: LuaTable = view.get("doc")?;
            if matches!(doc.get::<LuaValue>("abs_filename")?, LuaValue::Nil) { return Ok(()); }
            let doc_state: LuaTable = m.get("doc_state")?;
            let state: LuaValue = doc_state.get(doc.clone())?;
            let state = match state { LuaValue::Table(t) => t, _ => return Ok(()) };
            let client: LuaTable = match state.get::<LuaValue>("client")? { LuaValue::Table(t) => t, _ => return Ok(()) };
            let caps: LuaValue = client.get("capabilities")?;
            let provider = match caps { LuaValue::Table(c) => match c.get::<LuaValue>("completionProvider")? { LuaValue::Table(t) => t, _ => return Ok(()) }, _ => return Ok(()) };
            let triggers: LuaTable = provider.get::<LuaTable>("triggerCharacters").unwrap_or_else(|_| lua.create_table().unwrap());
            if triggers.raw_len() == 0 { return Ok(()); }
            let trigger_text = if text == ":" {
                let sel: LuaMultiValue = doc.get::<LuaFunction>("get_selection")?.call(doc.clone())?;
                let mut it = sel.into_iter();
                let line = match it.next() { Some(LuaValue::Integer(n)) => n, _ => 1 };
                let col  = match it.next() { Some(LuaValue::Integer(n)) => n, _ => 1 };
                if col > 1 {
                    let prev: String = doc.get::<LuaFunction>("get_char")?.call((doc, line, col - 1))?;
                    if prev == ":" { "::".to_owned() } else { text }
                } else { text }
            } else { text };
            let triggered = triggers.sequence_values::<String>().any(|r| r.map(|s| s == trigger_text).unwrap_or(false));
            if triggered { m.get::<LuaFunction>("complete")?.call::<()>(())? }
            Ok(())
        })?)?;
    }

    // ── format_document_for / format_document / format_selection ──────────────
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("format_document_for", lua.create_function(move |lua, (doc_arg, callback): (LuaValue, LuaValue)| {
            let m: LuaTable = lua.registry_value(&mk)?;
            let cb: LuaFunction = match callback { LuaValue::Function(f) => f, _ => lua.create_function(|_lua, _: LuaValue| Ok(()))? };
            let target_doc: LuaTable = match doc_arg {
                LuaValue::Table(t) => t,
                _ => match mgr_current_docview(lua)? {
                    Some(v) => v.get("doc")?,
                    None => { cb.call::<()>(false)?; return Ok(()); }
                },
            };
            if matches!(target_doc.get::<LuaValue>("abs_filename")?, LuaValue::Nil) { cb.call::<()>(false)?; return Ok(()); }
            let client_val: LuaValue = m.get::<LuaFunction>("open_doc")?.call(target_doc.clone())?;
            let client = match client_val { LuaValue::Table(t) => t, _ => { cb.call::<()>(false)?; return Ok(()); } };
            let abs: String = target_doc.get("abs_filename")?;
            let uri = mgr_path_to_uri(lua, &abs)?;
            let indent_info: LuaMultiValue = target_doc.get::<LuaFunction>("get_indent_info")?.call(target_doc.clone())?;
            let mut it = indent_info.into_iter();
            let indent_type: String = match it.next() { Some(LuaValue::String(s)) => s.to_str()?.to_owned(), _ => "soft".to_owned() };
            let tab_size: i64 = match it.next() { Some(LuaValue::Integer(n)) => n, _ => 4 };
            let td = lua.create_table()?; td.set("uri", uri)?;
            let format_opts = lua.create_table()?;
            format_opts.set("tabSize", tab_size)?;
            format_opts.set("insertSpaces", indent_type != "hard")?;
            let params = lua.create_table()?;
            params.set("textDocument", td)?; params.set("options", format_opts)?;
            let doc_c = target_doc.clone();
            let format_cb = lua.create_function(move |lua, (result, err): (LuaValue, LuaValue)| {
                if !matches!(err, LuaValue::Nil) { cb.call::<()>(false)?; return Ok(()); }
                let edits = match result { LuaValue::Table(t) => t, _ => lua.create_table()? };
                mgr_range_sort_desc(lua, &edits)?;
                for item in edits.sequence_values::<LuaTable>() {
                    let edit = item?;
                    mgr_apply_text_edit(lua, &doc_c, &edit, false)?;
                }
                cb.call::<()>(true)
            })?;
            client.get::<LuaFunction>("request")?.call::<()>((client, "textDocument/formatting", params, format_cb))
        })?)?;
    }
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("format_document", lua.create_function(move |lua, ()| {
            let m: LuaTable = lua.registry_value(&mk)?;
            let view = match mgr_current_docview(lua)? { Some(v) => v, None => return Ok(()) };
            let doc: LuaTable = view.get("doc")?;
            if matches!(doc.get::<LuaValue>("abs_filename")?, LuaValue::Nil) { return Ok(()); }
            m.get::<LuaFunction>("format_document_for")?.call::<()>((LuaValue::Table(doc), LuaValue::Nil))
        })?)?;
    }
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("format_selection", lua.create_function(move |lua, ()| {
            let m: LuaTable = lua.registry_value(&mk)?;
            let view = match mgr_current_docview(lua)? { Some(v) => v, None => return Ok(()) };
            let doc: LuaTable = view.get("doc")?;
            if matches!(doc.get::<LuaValue>("abs_filename")?, LuaValue::Nil) { return Ok(()); }
            let client_val: LuaValue = m.get::<LuaFunction>("open_doc")?.call(doc.clone())?;
            let client = match client_val { LuaValue::Table(t) => t, _ => return Ok(()) };
            let sel: LuaMultiValue = doc.get::<LuaFunction>("get_selection")?.call((doc.clone(), true))?;
            let mut it = sel.into_iter();
            let l1 = match it.next() { Some(LuaValue::Integer(n)) => n, _ => 1 };
            let c1 = match it.next() { Some(LuaValue::Integer(n)) => n, _ => 1 };
            let l2 = match it.next() { Some(LuaValue::Integer(n)) => n, _ => l1 };
            let c2 = match it.next() { Some(LuaValue::Integer(n)) => n, _ => c1 };
            let abs: String = doc.get("abs_filename")?;
            let uri = mgr_path_to_uri(lua, &abs)?;
            let indent_info: LuaMultiValue = doc.get::<LuaFunction>("get_indent_info")?.call(doc.clone())?;
            let mut ii = indent_info.into_iter();
            let indent_type: String = match ii.next() { Some(LuaValue::String(s)) => s.to_str()?.to_owned(), _ => "soft".to_owned() };
            let tab_size: i64 = match ii.next() { Some(LuaValue::Integer(n)) => n, _ => 4 };
            let td = lua.create_table()?; td.set("uri", uri)?;
            let range_t = lua.create_table()?;
            range_t.set("start", mgr_lsp_pos(lua, &doc, l1, c1)?)?;
            range_t.set("end",   mgr_lsp_pos(lua, &doc, l2, c2)?)?;
            let fopts = lua.create_table()?;
            fopts.set("tabSize", tab_size)?; fopts.set("insertSpaces", indent_type != "hard")?;
            let params = lua.create_table()?;
            params.set("textDocument", td)?; params.set("range", range_t)?; params.set("options", fopts)?;
            let doc_c = doc.clone();
            let format_cb = lua.create_function(move |lua, (result, err): (LuaValue, LuaValue)| {
                if !matches!(err, LuaValue::Nil) { return Ok(()); }
                let edits = match result { LuaValue::Table(t) => t, _ => return Ok(()) };
                mgr_range_sort_desc(lua, &edits)?;
                for item in edits.sequence_values::<LuaTable>() {
                    mgr_apply_text_edit(lua, &doc_c, &item?, false)?;
                }
                Ok(())
            })?;
            client.get::<LuaFunction>("request")?.call::<()>((client, "textDocument/rangeFormatting", params, format_cb))
        })?)?;
    }

    // ── workspace_symbols ──────────────────────────────────────────────────────
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("workspace_symbols", lua.create_function(move |lua, ()| {
            let m: LuaTable = lua.registry_value(&mk)?;
            let view = mgr_current_docview(lua)?;
            let doc_opt: Option<LuaTable> = view.as_ref().and_then(|v| v.get::<LuaTable>("doc").ok());
            let client_val: LuaValue = match &doc_opt {
                Some(d) => m.get::<LuaFunction>("open_doc")?.call(d.clone())?,
                None => LuaValue::Nil,
            };
            let client = match client_val { LuaValue::Table(t) => t, _ => {
                let core: LuaTable = req(lua, "core")?;
                return core.get::<LuaFunction>("warn")?.call::<()>("No LSP server configured for workspace symbol search");
            }};
            let mk2 = Arc::clone(&mk);
            let submit_fn = lua.create_function(move |lua, text: String| {
                if text.is_empty() { return Ok(()); }
                let params = lua.create_table()?; params.set("query", text)?;
                let mk3 = Arc::clone(&mk2);
                let cb = lua.create_function(move |lua, (result, err): (LuaValue, LuaValue)| {
                    if !matches!(err, LuaValue::Nil) { return Ok(()); }
                    let result = match result { LuaValue::Table(t) => t, _ => lua.create_table()? };
                    let items = lua.create_table()?;
                    for (i, item) in result.sequence_values::<LuaTable>().enumerate() {
                        let symbol = item?;
                        let loc: LuaValue = symbol.get("location").unwrap_or(LuaValue::Nil);
                        let (uri_v, range_v) = mgr_location_to_target(if let LuaValue::Table(t) = &loc { t } else { &symbol })?;
                        let uri = match &uri_v { LuaValue::String(s) => s.to_str()?.to_owned(), _ => continue };
                        let range = match range_v { LuaValue::Table(t) => t, _ => continue };
                        let name: String = symbol.get("name").unwrap_or_default();
                        let container: String = symbol.get("containerName").unwrap_or_default();
                        let payload = lua.create_table()?; payload.set("uri", uri)?; payload.set("range", range)?;
                        let entry = lua.create_table()?;
                        entry.set("text", format!("{:03} {}", i + 1, name))?;
                        entry.set("info", container)?; entry.set("payload", payload)?;
                        items.raw_set(items.raw_len() + 1, entry)?;
                    }
                    let mk4 = Arc::clone(&mk3);
                    mgr_pick_from_list(lua, "Workspace Symbols", items, lua.create_function(move |lua, item: LuaTable| {
                        let m: LuaTable = lua.registry_value(&mk4)?;
                        let uri: String = item.get("uri")?; let range: LuaTable = item.get("range")?;
                        mgr_open_location(lua, &m, &uri, &range, None)
                    })?)
                })?;
                client.get::<LuaFunction>("request")?.call::<()>((client.clone(), "workspace/symbol", params, cb))
            })?;
            let opts = lua.create_table()?;
            opts.set("submit", submit_fn)?;
            opts.set("suggest", lua.create_function(|lua, _: LuaValue| lua.create_table())?)?;
            opts.set("show_suggestions", false)?;
            let core: LuaTable = req(lua, "core")?;
            let cv: LuaTable = core.get("command_view")?;
            cv.get::<LuaFunction>("enter")?.call::<()>((cv, "Workspace Symbols", opts))
        })?)?;
    }

    // ── next_diagnostic / previous_diagnostic ──────────────────────────────────
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("next_diagnostic", lua.create_function(move |lua, ()| {
            let m: LuaTable = lua.registry_value(&mk)?;
            let view = match mgr_current_docview(lua)? { Some(v) => v, None => return Ok(()) };
            let doc: LuaTable = view.get("doc")?;
            if matches!(doc.get::<LuaValue>("abs_filename")?, LuaValue::Nil) { return Ok(()); }
            let diagnostics = mgr_get_sorted_doc_diagnostics(lua, &doc)?;
            if diagnostics.raw_len() == 0 {
                let core: LuaTable = req(lua, "core")?;
                let name: String = doc.get::<LuaFunction>("get_name")?.call(doc)?;
                return core.get::<LuaFunction>("warn")?.call::<()>(format!("No LSP diagnostics for {}", name));
            }
            let sel: LuaMultiValue = doc.get::<LuaFunction>("get_selection")?.call(doc.clone())?;
            let mut it = sel.into_iter();
            let line = match it.next() { Some(LuaValue::Integer(n)) => n, _ => 1 };
            let col  = match it.next() { Some(LuaValue::Integer(n)) => n, _ => 1 };
            let lines_t: LuaTable = doc.get("lines")?;
            let line_text: String = lines_t.get::<String>(line).unwrap_or_else(|_| "\n".to_owned());
            let current_line = line - 1;
            let current_char = mgr_byte_to_utf8_char(&line_text, col as usize) as i64;
            let mut target: Option<LuaTable> = None;
            for item in diagnostics.sequence_values::<LuaTable>() {
                let d = item?;
                if let Ok(LuaValue::Table(r)) = d.get::<LuaValue>("range") {
                    if let Ok(start) = r.get::<LuaTable>("start") {
                        let dl = start.get::<i64>("line").unwrap_or(0);
                        let dc = start.get::<i64>("character").unwrap_or(0);
                        if dl > current_line || (dl == current_line && dc > current_char) {
                            target = Some(d); break;
                        }
                    }
                }
            }
            if target.is_none() { target = diagnostics.get::<LuaTable>(1).ok(); }
            if let Some(d) = target {
                let abs: String = doc.get("abs_filename")?;
                let uri = mgr_path_to_uri(lua, &abs)?;
                let range: LuaTable = d.get::<LuaTable>("range")?;
                mgr_open_location(lua, &m, &uri, &range, None)?;
            }
            Ok(())
        })?)?;
    }
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("previous_diagnostic", lua.create_function(move |lua, ()| {
            let m: LuaTable = lua.registry_value(&mk)?;
            let view = match mgr_current_docview(lua)? { Some(v) => v, None => return Ok(()) };
            let doc: LuaTable = view.get("doc")?;
            if matches!(doc.get::<LuaValue>("abs_filename")?, LuaValue::Nil) { return Ok(()); }
            let diagnostics = mgr_get_sorted_doc_diagnostics(lua, &doc)?;
            if diagnostics.raw_len() == 0 {
                let core: LuaTable = req(lua, "core")?;
                let name: String = doc.get::<LuaFunction>("get_name")?.call(doc)?;
                return core.get::<LuaFunction>("warn")?.call::<()>(format!("No LSP diagnostics for {}", name));
            }
            let sel: LuaMultiValue = doc.get::<LuaFunction>("get_selection")?.call(doc.clone())?;
            let mut it = sel.into_iter();
            let line = match it.next() { Some(LuaValue::Integer(n)) => n, _ => 1 };
            let col  = match it.next() { Some(LuaValue::Integer(n)) => n, _ => 1 };
            let lines_t: LuaTable = doc.get("lines")?;
            let line_text: String = lines_t.get::<String>(line).unwrap_or_else(|_| "\n".to_owned());
            let current_line = line - 1;
            let current_char = mgr_byte_to_utf8_char(&line_text, col as usize) as i64;
            let count = diagnostics.raw_len();
            let mut target: Option<LuaTable> = None;
            for i in (1..=count).rev() {
                let d: LuaTable = diagnostics.get(i)?;
                if let Ok(LuaValue::Table(r)) = d.get::<LuaValue>("range") {
                    if let Ok(start) = r.get::<LuaTable>("start") {
                        let dl = start.get::<i64>("line").unwrap_or(0);
                        let dc = start.get::<i64>("character").unwrap_or(0);
                        if dl < current_line || (dl == current_line && dc < current_char) {
                            target = Some(d); break;
                        }
                    }
                }
            }
            if target.is_none() { target = diagnostics.get::<LuaTable>(count).ok(); }
            if let Some(d) = target {
                let abs: String = doc.get("abs_filename")?;
                let uri = mgr_path_to_uri(lua, &abs)?;
                let range: LuaTable = d.get::<LuaTable>("range")?;
                mgr_open_location(lua, &m, &uri, &range, None)?;
            }
            Ok(())
        })?)?;
    }

    // ── refresh_semantic_highlighting / complete / rename_symbol / restart ─────
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("refresh_semantic_highlighting", lua.create_function(move |lua, ()| {
            let m: LuaTable = lua.registry_value(&mk)?;
            let view = match mgr_current_docview(lua)? { Some(v) => v, None => return Ok(()) };
            let doc: LuaTable = view.get("doc")?;
            m.get::<LuaFunction>("request_semantic_tokens")?.call::<()>(doc)
        })?)?;
    }
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("complete", lua.create_function(move |lua, ()| {
            let m: LuaTable = lua.registry_value(&mk)?;
            let view = match mgr_current_docview(lua)? { Some(v) => v, None => return Ok(()) };
            let doc: LuaTable = view.get("doc")?;
            if matches!(doc.get::<LuaValue>("abs_filename")?, LuaValue::Nil) { return Ok(()); }
            let client_val: LuaValue = m.get::<LuaFunction>("open_doc")?.call(doc.clone())?;
            let client = match client_val { LuaValue::Table(t) => t, _ => return Ok(()) };
            let params: LuaTable = m.get::<LuaFunction>("document_params")?.call(doc)?;
            let cb = lua.create_function(move |lua, (result, err): (LuaValue, LuaValue)| {
                if !matches!(err, LuaValue::Nil) { return Ok(()); }
                let items_table = match result { LuaValue::Table(t) => { if matches!(t.get::<LuaValue>("items")?, LuaValue::Table(_)) { t.get::<LuaTable>("items")? } else { t } } _ => lua.create_table()? };
                let out = lua.create_table()?;
                out.set("name", "lsp")?;
                let out_items = lua.create_table()?;
                let protocol: LuaTable = req(lua, "plugins.lsp.protocol")?;
                let completion_kinds: LuaTable = protocol.get("completion_kinds").unwrap_or_else(|_| lua.create_table().unwrap());
                for item in items_table.sequence_values::<LuaTable>() {
                    let item = item?;
                    let label: LuaValue = item.get("label")?;
                    if matches!(label, LuaValue::Nil) { continue; }
                    let label_str = match &label { LuaValue::String(s) => s.to_str()?.to_owned(), _ => continue };
                    let insert_text: String = item.get::<String>("insertText")
                        .or_else(|_| item.get::<LuaTable>("textEdit").and_then(|te| te.get("newText")))
                        .unwrap_or_else(|_| label_str.clone());
                    let detail: LuaValue = item.get("detail").unwrap_or(LuaValue::Nil);
                    let doc_val: LuaValue = item.get("documentation").unwrap_or(LuaValue::Nil);
                    let desc = mgr_content_to_text(&doc_val)?;
                    let kind: LuaValue = item.get("kind").unwrap_or(LuaValue::Nil);
                    let icon: String = match &kind { LuaValue::Integer(k) => completion_kinds.get::<String>(*k).unwrap_or_else(|_| "keyword".to_owned()), _ => "keyword".to_owned() };
                    let entry = lua.create_table()?;
                    entry.set("info", detail)?;
                    entry.set("desc", desc)?;
                    entry.set("icon", icon)?;
                    entry.set("data", item.clone())?;
                    let item_c = item.clone();
                    let onselect = lua.create_function(move |lua, (_self, selected): (LuaValue, LuaTable)| {
                        let view = match mgr_current_docview(lua)? { Some(v) => v, None => return Ok(false) };
                        let doc: LuaTable = view.get("doc")?;
                        let selected_item = &item_c;
                        if let Ok(LuaValue::Table(text_edit)) = selected_item.get::<LuaValue>("textEdit") {
                            mgr_apply_text_edit(lua, &doc, &text_edit, true)?;
                            if let Ok(LuaValue::Table(additional)) = selected_item.get::<LuaValue>("additionalTextEdits") {
                                mgr_range_sort_desc(lua, &additional)?;
                                for edit in additional.sequence_values::<LuaTable>() {
                                    mgr_apply_text_edit(lua, &doc, &edit?, false)?;
                                }
                            }
                            return Ok(true);
                        }
                        selected.set("text", insert_text.clone())?;
                        Ok(false)
                    })?;
                    entry.set("onselect", onselect)?;
                    out_items.set(label_str, entry)?;
                }
                out.set("items", out_items)?;
                let autocomplete: LuaTable = req(lua, "plugins.autocomplete")?;
                autocomplete.get::<LuaFunction>("complete")?.call::<()>((autocomplete, out))
            })?;
            client.get::<LuaFunction>("request")?.call::<()>((client, "textDocument/completion", params, cb))
        })?)?;
    }
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("rename_symbol", lua.create_function(move |lua, ()| {
            let m: LuaTable = lua.registry_value(&mk)?;
            let view = match mgr_current_docview(lua)? { Some(v) => v, None => return Ok(()) };
            let doc: LuaTable = view.get("doc")?;
            if matches!(doc.get::<LuaValue>("abs_filename")?, LuaValue::Nil) { return Ok(()); }
            let client_val: LuaValue = m.get::<LuaFunction>("open_doc")?.call(doc.clone())?;
            let client = match client_val { LuaValue::Table(t) => t, _ => return Ok(()) };
            let sel: LuaMultiValue = doc.get::<LuaFunction>("get_selection")?.call((doc.clone(), true))?;
            let mut it = sel.into_iter();
            let l1 = match it.next() { Some(LuaValue::Integer(n)) => n, _ => 1 };
            let c1 = match it.next() { Some(LuaValue::Integer(n)) => n, _ => 1 };
            let l2 = match it.next() { Some(LuaValue::Integer(n)) => n, _ => l1 };
            let c2 = match it.next() { Some(LuaValue::Integer(n)) => n, _ => c1 };
            let current_name: String = doc.get::<LuaFunction>("get_text")?.call((doc.clone(), l1, c1, l2, c2))?;
            let doc_c = doc.clone();
            let client_c = client.clone();
            let mk2 = Arc::clone(&mk);
            let current_name2 = current_name.clone();
            let opts = lua.create_table()?;
            opts.set("text", current_name.clone())?;
            opts.set("select_text", true)?;
            opts.set("suggest", lua.create_function(move |lua, text: String| {
                let core: LuaTable = req(lua, "core")?;
                let sv: LuaTable = core.get("status_view")?;
                sv.get::<LuaFunction>("show_tooltip")?.call::<()>((sv, format!("{} -> {}", current_name2, text)))?;
                lua.create_table()
            })?)?;
            opts.set("submit", lua.create_function(move |lua, text: String| {
                let m: LuaTable = lua.registry_value(&mk2)?;
                let core: LuaTable = req(lua, "core")?;
                let sv: LuaTable = core.get("status_view")?;
                sv.get::<LuaFunction>("remove_tooltip")?.call::<()>((sv,))?;
                if text.is_empty() { return Ok(()); }
                let params: LuaTable = m.get::<LuaFunction>("document_params")?.call(doc_c.clone())?;
                params.set("newName", text)?;
                let cb = lua.create_function(move |lua, (result, err): (LuaValue, LuaValue)| {
                    if !matches!(err, LuaValue::Nil) { return Ok(()); }
                    mgr_apply_workspace_edit(lua, result)
                })?;
                client_c.get::<LuaFunction>("request")?.call::<()>((client_c.clone(), "textDocument/rename", params, cb))
            })?)?;
            opts.set("cancel", lua.create_function(|lua, ()| {
                let core: LuaTable = req(lua, "core")?;
                let sv: LuaTable = core.get("status_view")?;
                sv.get::<LuaFunction>("remove_tooltip")?.call::<()>((sv,))
            })?)?;
            let core: LuaTable = req(lua, "core")?;
            let cv: LuaTable = core.get("command_view")?;
            cv.get::<LuaFunction>("enter")?.call::<()>((cv, "Rename symbol to", opts))
        })?)?;
    }
    {
        let mk = Arc::clone(&mgr_key);
        mgr.set("restart", lua.create_function(move |lua, ()| {
            let m: LuaTable = lua.registry_value(&mk)?;
            let clients: LuaTable = m.get("clients")?;
            for pair in clients.clone().pairs::<LuaValue, LuaTable>() {
                let (_, client) = pair?;
                let _ = client.get::<LuaFunction>("shutdown").and_then(|f| f.call::<()>((client.clone(),)));
            }
            m.get::<LuaFunction>("reload_config")?.call::<()>(())?;
            let core: LuaTable = req(lua, "core")?;
            let docs: LuaTable = core.get("docs")?;
            for item in docs.sequence_values::<LuaTable>() {
                let doc = item?;
                if !matches!(doc.get::<LuaValue>("abs_filename")?, LuaValue::Nil) {
                    let _ = m.get::<LuaFunction>("open_doc")?.call::<LuaValue>(doc);
                }
            }
            Ok(())
        })?)?;
    }

    // ── autocomplete provider + commands ───────────────────────────────────────
    {
        let mk = Arc::clone(&mgr_key);
        let autocomplete: LuaTable = req(lua, "plugins.autocomplete")?;
        let mk_provider = Arc::clone(&mgr_key);
        let provider_fn = lua.create_function(move |lua, (ctx, respond): (LuaTable, LuaFunction)| {
            let m: LuaTable = lua.registry_value(&mk_provider)?;
            let doc: LuaValue = ctx.get("doc")?;
            let doc = match doc { LuaValue::Table(t) => t, _ => { respond.call::<()>(lua.create_table()?)?; return Ok(()); } };
            if matches!(doc.get::<LuaValue>("abs_filename")?, LuaValue::Nil) {
                respond.call::<()>(lua.create_table()?)?; return Ok(());
            }
            let client_val: LuaValue = m.get::<LuaFunction>("open_doc")?.call(doc.clone())?;
            let client = match client_val { LuaValue::Table(t) => t, _ => { respond.call::<()>(lua.create_table()?)?; return Ok(()); } };
            let line: Option<i64> = ctx.get("line").ok();
            let col:  Option<i64> = ctx.get("col").ok();
            let params: LuaTable = m.get::<LuaFunction>("document_params")?.call((doc, line, col))?;
            let cb = lua.create_function(move |lua, (result, err): (LuaValue, LuaValue)| {
                if !matches!(err, LuaValue::Nil) { respond.call::<()>(lua.create_table()?)?; return Ok(()); }
                // Re-use complete's conversion logic inline
                let items_table = match result { LuaValue::Table(t) => { if matches!(t.get::<LuaValue>("items")?, LuaValue::Table(_)) { t.get::<LuaTable>("items")? } else { t } } _ => lua.create_table()? };
                let out = lua.create_table()?; out.set("name", "lsp")?;
                let out_items = lua.create_table()?;
                let protocol: LuaTable = req(lua, "plugins.lsp.protocol")?;
                let completion_kinds: LuaTable = protocol.get("completion_kinds").unwrap_or_else(|_| lua.create_table().unwrap());
                for item in items_table.sequence_values::<LuaTable>() {
                    let item = item?;
                    let label: LuaValue = item.get("label")?;
                    let label_str = match &label { LuaValue::String(s) => s.to_str()?.to_owned(), _ => continue };
                    let insert_text: String = item.get::<String>("insertText")
                        .or_else(|_| item.get::<LuaTable>("textEdit").and_then(|te| te.get("newText")))
                        .unwrap_or_else(|_| label_str.clone());
                    let detail: LuaValue = item.get("detail").unwrap_or(LuaValue::Nil);
                    let doc_val: LuaValue = item.get("documentation").unwrap_or(LuaValue::Nil);
                    let desc = mgr_content_to_text(&doc_val)?;
                    let kind: LuaValue = item.get("kind").unwrap_or(LuaValue::Nil);
                    let icon: String = match &kind { LuaValue::Integer(k) => completion_kinds.get::<String>(*k).unwrap_or_else(|_| "keyword".to_owned()), _ => "keyword".to_owned() };
                    let entry = lua.create_table()?;
                    entry.set("info", detail)?; entry.set("desc", desc)?; entry.set("icon", icon)?;
                    entry.set("data", item.clone())?;
                    let item_c = item.clone();
                    let onselect = lua.create_function(move |lua, (_self, selected): (LuaValue, LuaTable)| {
                        let view = match mgr_current_docview(lua)? { Some(v) => v, None => return Ok(false) };
                        let doc: LuaTable = view.get("doc")?;
                        if let Ok(LuaValue::Table(te)) = item_c.get::<LuaValue>("textEdit") {
                            mgr_apply_text_edit(lua, &doc, &te, true)?;
                            if let Ok(LuaValue::Table(add)) = item_c.get::<LuaValue>("additionalTextEdits") {
                                mgr_range_sort_desc(lua, &add)?;
                                for edit in add.sequence_values::<LuaTable>() { mgr_apply_text_edit(lua, &doc, &edit?, false)?; }
                            }
                            return Ok(true);
                        }
                        selected.set("text", insert_text.clone())?;
                        Ok(false)
                    })?;
                    entry.set("onselect", onselect)?;
                    out_items.set(label_str, entry)?;
                }
                out.set("items", out_items)?;
                respond.call::<()>(out)
            })?;
            client.get::<LuaFunction>("request")?.call::<()>((client, "textDocument/completion", params, cb))
        })?;
        autocomplete.get::<LuaFunction>("register_provider")?.call::<()>(("lsp", provider_fn))?;
        autocomplete.get::<LuaFunction>("set_default_mode")?.call::<()>("lsp")?;

        // Commands
        let command: LuaTable = req(lua, "core.command")?;
        let docview_pred = lua.create_function(move |lua, ()| -> LuaResult<LuaMultiValue> {
            let view = match mgr_current_docview(lua)? {
                Some(v) => v, None => return Ok(LuaMultiValue::from_vec(vec![LuaValue::Boolean(false)])),
            };
            let doc: LuaTable = view.get("doc")?;
            if matches!(doc.get::<LuaValue>("abs_filename")?, LuaValue::Nil) {
                return Ok(LuaMultiValue::from_vec(vec![LuaValue::Boolean(false)]));
            }
            Ok(LuaMultiValue::from_vec(vec![LuaValue::Boolean(true), LuaValue::Table(view)]))
        })?;
        let mk_c = Arc::clone(&mk);
        let cmds = lua.create_table()?;
        {
            let mk3 = Arc::clone(&mk_c);
            cmds.set("lsp:next-diagnostic",            lua.create_function(move |lua, ()| lua.registry_value::<LuaTable>(&mk3)?.get::<LuaFunction>("next_diagnostic")?.call::<()>(()))?)?;
        }
        {
            let mk3 = Arc::clone(&mk_c);
            cmds.set("lsp:previous-diagnostic",        lua.create_function(move |lua, ()| lua.registry_value::<LuaTable>(&mk3)?.get::<LuaFunction>("previous_diagnostic")?.call::<()>(()))?)?;
        }
        {
            let mk3 = Arc::clone(&mk_c);
            cmds.set("lsp:signature-help",             lua.create_function(move |lua, ()| lua.registry_value::<LuaTable>(&mk3)?.get::<LuaFunction>("signature_help")?.call::<()>(()))?)?;
        }
        {
            let mk3 = Arc::clone(&mk_c);
            cmds.set("lsp:format-document",            lua.create_function(move |lua, ()| lua.registry_value::<LuaTable>(&mk3)?.get::<LuaFunction>("format_document")?.call::<()>(()))?)?;
        }
        {
            let mk3 = Arc::clone(&mk_c);
            cmds.set("lsp:format-selection",           lua.create_function(move |lua, ()| lua.registry_value::<LuaTable>(&mk3)?.get::<LuaFunction>("format_selection")?.call::<()>(()))?)?;
        }
        {
            let mk3 = Arc::clone(&mk_c);
            cmds.set("lsp:hover",                      lua.create_function(move |lua, ()| lua.registry_value::<LuaTable>(&mk3)?.get::<LuaFunction>("hover")?.call::<()>(()))?)?;
        }
        {
            let mk3 = Arc::clone(&mk_c);
            cmds.set("lsp:complete",                   lua.create_function(move |lua, ()| lua.registry_value::<LuaTable>(&mk3)?.get::<LuaFunction>("complete")?.call::<()>(()))?)?;
        }
        {
            let mk3 = Arc::clone(&mk_c);
            cmds.set("lsp:rename-symbol",              lua.create_function(move |lua, ()| lua.registry_value::<LuaTable>(&mk3)?.get::<LuaFunction>("rename_symbol")?.call::<()>(()))?)?;
        }
        {
            let mk3 = Arc::clone(&mk_c);
            cmds.set("lsp:show-diagnostics",           lua.create_function(move |lua, ()| lua.registry_value::<LuaTable>(&mk3)?.get::<LuaFunction>("show_diagnostics")?.call::<()>(()))?)?;
        }
        {
            let mk3 = Arc::clone(&mk_c);
            cmds.set("lsp:refresh-semantic-highlighting", lua.create_function(move |lua, ()| lua.registry_value::<LuaTable>(&mk3)?.get::<LuaFunction>("refresh_semantic_highlighting")?.call::<()>(()))?)?;
        }
        {
            let mk3 = Arc::clone(&mk_c);
            cmds.set("lsp:workspace-symbols",          lua.create_function(move |lua, ()| lua.registry_value::<LuaTable>(&mk3)?.get::<LuaFunction>("workspace_symbols")?.call::<()>(()))?)?;
        }
        command.get::<LuaFunction>("add")?.call::<()>((docview_pred, cmds))?;

        // Navigation commands with capability predicate
        let nav_cmds_def: &[(&str, &str, &str)] = &[
            ("lsp:goto-definition",      "goto_definition",      "definitionProvider"),
            ("lsp:goto-type-definition", "goto_type_definition", "typeDefinitionProvider"),
            ("lsp:goto-implementation",  "goto_implementation",  "implementationProvider"),
            ("lsp:find-references",      "find_references",      "referencesProvider"),
            ("lsp:show-document-symbols","show_document_symbols","documentSymbolProvider"),
        ];
        for (cmd_name, method_name, capability) in nav_cmds_def {
            let cap_owned = capability.to_string();
            let mk3 = Arc::clone(&mgr_key);
            let pred = lua.create_function(move |lua, ()| -> LuaResult<LuaMultiValue> {
                let m: LuaTable = lua.registry_value(&mk3)?;
                let view = match mgr_current_docview(lua)? {
                    Some(v) => v, None => return Ok(LuaMultiValue::from_vec(vec![LuaValue::Boolean(false)])),
                };
                let doc: LuaTable = view.get("doc")?;
                if matches!(doc.get::<LuaValue>("abs_filename")?, LuaValue::Nil) {
                    return Ok(LuaMultiValue::from_vec(vec![LuaValue::Boolean(false)]));
                }
                let spec_val: LuaValue = m.get::<LuaFunction>("find_spec_for_doc")?.call(doc.clone())?;
                if matches!(spec_val, LuaValue::Nil) {
                    return Ok(LuaMultiValue::from_vec(vec![LuaValue::Boolean(false)]));
                }
                let doc_state: LuaTable = m.get("doc_state")?;
                if let Ok(LuaValue::Table(state)) = doc_state.get::<LuaValue>(doc) {
                    if let Ok(LuaValue::Table(client)) = state.get::<LuaValue>("client") {
                        if client.get::<bool>("is_initialized").unwrap_or(false)
                            && !mgr_cap_supported(&client, &cap_owned)?
                        {
                            return Ok(LuaMultiValue::from_vec(vec![LuaValue::Boolean(false)]));
                        }
                    }
                }
                Ok(LuaMultiValue::from_vec(vec![LuaValue::Boolean(true), LuaValue::Table(view)]))
            })?;
            let mk4 = Arc::clone(&mgr_key);
            let method_owned = method_name.to_string();
            let nav_cmd = lua.create_table()?;
            nav_cmd.set(*cmd_name, lua.create_function(move |lua, ()| {
                lua.registry_value::<LuaTable>(&mk4)?.get::<LuaFunction>(&method_owned[..])?.call::<()>(())
            })?)?;
            command.get::<LuaFunction>("add")?.call::<()>((pred, nav_cmd))?;
        }
        // code-action + quick-fix with capability predicate
        {
            let mk3 = Arc::clone(&mgr_key);
            let pred = lua.create_function(move |lua, ()| -> LuaResult<LuaMultiValue> {
                let m: LuaTable = lua.registry_value(&mk3)?;
                let view = match mgr_current_docview(lua)? {
                    Some(v) => v, None => return Ok(LuaMultiValue::from_vec(vec![LuaValue::Boolean(false)])),
                };
                let doc: LuaTable = view.get("doc")?;
                if matches!(doc.get::<LuaValue>("abs_filename")?, LuaValue::Nil) {
                    return Ok(LuaMultiValue::from_vec(vec![LuaValue::Boolean(false)]));
                }
                let spec_val: LuaValue = m.get::<LuaFunction>("find_spec_for_doc")?.call(doc.clone())?;
                if matches!(spec_val, LuaValue::Nil) { return Ok(LuaMultiValue::from_vec(vec![LuaValue::Boolean(false)])); }
                let doc_state: LuaTable = m.get("doc_state")?;
                if let Ok(LuaValue::Table(state)) = doc_state.get::<LuaValue>(doc) {
                    if let Ok(LuaValue::Table(client)) = state.get::<LuaValue>("client") {
                        if client.get::<bool>("is_initialized").unwrap_or(false) && !mgr_cap_supported(&client, "codeActionProvider")? {
                            return Ok(LuaMultiValue::from_vec(vec![LuaValue::Boolean(false)]));
                        }
                    }
                }
                Ok(LuaMultiValue::from_vec(vec![LuaValue::Boolean(true), LuaValue::Table(view)]))
            })?;
            let mk4a = Arc::clone(&mgr_key);
            let mk4b = Arc::clone(&mgr_key);
            let ca_cmds = lua.create_table()?;
            ca_cmds.set("lsp:code-action", lua.create_function(move |lua, ()| lua.registry_value::<LuaTable>(&mk4a)?.get::<LuaFunction>("code_action")?.call::<()>(()))?)?;
            ca_cmds.set("lsp:quick-fix",   lua.create_function(move |lua, ()| lua.registry_value::<LuaTable>(&mk4b)?.get::<LuaFunction>("quick_fix")?.call::<()>(()))?)?;
            command.get::<LuaFunction>("add")?.call::<()>((pred, ca_cmds))?;
        }
        // Global commands (nil predicate)
        {
            let mk3a = Arc::clone(&mgr_key);
            let mk3b = Arc::clone(&mgr_key);
            let global_cmds = lua.create_table()?;
            global_cmds.set("lsp:jump-back", lua.create_function(move |lua, ()| lua.registry_value::<LuaTable>(&mk3a)?.get::<LuaFunction>("jump_back")?.call::<bool>(()))?)?;
            global_cmds.set("lsp:restart",   lua.create_function(move |lua, ()| lua.registry_value::<LuaTable>(&mk3b)?.get::<LuaFunction>("restart")?.call::<()>(()))?)?;
            command.get::<LuaFunction>("add")?.call::<()>((LuaValue::Nil, global_cmds))?;
        }
    }

    Ok(LuaValue::Table(mgr))
}

// ─── CLIENT MODULE ────────────────────────────────────────────────────────────

/// Pure-Rust replacement for CLIENT_SOURCE — builds the Client class table.
fn init_client_module(lua: &Lua) -> LuaResult<LuaValue> {
    let class = lua.create_table()?;
    class.set("__index", class.clone())?;

    class.set("is_running", lua.create_function(|lua, self_: LuaTable| {
        let tid: i64 = self_.get("transport_id")?;
        let native: LuaTable = req(lua, "lsp_transport")?;
        let poll: LuaFunction = native.get("poll")?;
        Ok(match poll.call::<LuaValue>((tid, 0i64)) {
            Ok(LuaValue::Table(s)) => s.get::<bool>("running").unwrap_or(false),
            _ => false,
        })
    })?)?;

    class.set("send", lua.create_function(|lua, (self_, msg): (LuaTable, LuaTable)| {
        let tid: i64 = self_.get("transport_id")?;
        let native: LuaTable = req(lua, "lsp_transport")?;
        let f: LuaFunction = native.get("send")?;
        Ok(f.call::<LuaValue>((tid, msg)).is_ok())
    })?)?;

    class.set("queue_or_send", lua.create_function(|lua, (self_, msg, bypass): (LuaTable, LuaTable, Option<bool>)| {
        if !bypass.unwrap_or(false) && !self_.get::<bool>("is_initialized").unwrap_or(false) {
            let q: LuaTable = self_.get("pre_init_queue")?;
            q.raw_set(q.raw_len() + 1, msg)?;
            return Ok(true);
        }
        let tid: i64 = self_.get("transport_id")?;
        let native: LuaTable = req(lua, "lsp_transport")?;
        let f: LuaFunction = native.get("send")?;
        Ok(f.call::<LuaValue>((tid, msg)).is_ok())
    })?)?;

    class.set("notify", lua.create_function(|lua, (self_, method, params, bypass): (LuaTable, String, LuaValue, Option<bool>)| {
        let msg = lua.create_table()?;
        msg.set("jsonrpc", "2.0")?;
        msg.set("method", method)?;
        msg.set("params", params)?;
        let f: LuaFunction = self_.get("queue_or_send")?;
        f.call::<bool>((self_, msg, bypass))
    })?)?;

    class.set("request", lua.create_function(|lua, (self_, method, params, cb, bypass): (LuaTable, String, LuaValue, Option<LuaFunction>, Option<bool>)| {
        let id: i64 = self_.get::<i64>("next_request_id").unwrap_or(0) + 1;
        self_.set("next_request_id", id)?;
        if let Some(f) = cb {
            let pending: LuaTable = self_.get("pending")?;
            pending.set(id, f)?;
        }
        let msg = lua.create_table()?;
        msg.set("jsonrpc", "2.0")?;
        msg.set("id", id)?;
        msg.set("method", method)?;
        msg.set("params", params)?;
        let f: LuaFunction = self_.get("queue_or_send")?;
        f.call::<bool>((self_, msg, bypass))
    })?)?;

    class.set("flush_pre_init_queue", lua.create_function(|lua, self_: LuaTable| {
        let q: LuaTable = self_.get("pre_init_queue")?;
        self_.set("pre_init_queue", lua.create_table()?)?;
        let tid: i64 = self_.get("transport_id")?;
        let native: LuaTable = req(lua, "lsp_transport")?;
        let f: LuaFunction = native.get("send")?;
        for msg in q.sequence_values::<LuaTable>() {
            let _ = f.call::<LuaValue>((tid, msg?));
        }
        Ok(())
    })?)?;

    class.set("handle_message", lua.create_function(|lua, (self_, message): (LuaTable, LuaTable)| {
        let id: LuaValue = message.get("id")?;
        if !matches!(id, LuaValue::Nil) {
            let pending: LuaTable = self_.get("pending")?;
            let cb: LuaValue = pending.get(id.clone())?;
            pending.set(id, LuaValue::Nil)?;
            if let LuaValue::Function(f) = cb {
                let core: LuaTable = req(lua, "core")?;
                let try_fn: LuaFunction = core.get("try")?;
                let result: LuaValue = message.get("result")?;
                let err: LuaValue = message.get("error")?;
                let _ = try_fn.call::<LuaValue>((f, result, err, message));
            }
            return Ok(());
        }
        let handlers: LuaTable = self_.get("handlers")?;
        let on_notif: LuaValue = handlers.get("on_notification")?;
        let method: LuaValue = message.get("method")?;
        if let LuaValue::Function(f) = on_notif {
            if !matches!(method, LuaValue::Nil) {
                let core: LuaTable = req(lua, "core")?;
                let try_fn: LuaFunction = core.get("try")?;
                let _ = try_fn.call::<LuaValue>((f, self_, message));
            }
        }
        Ok(())
    })?)?;

    class.set("initialize", lua.create_function(|lua, (self_, params, on_ready): (LuaTable, LuaTable, Option<LuaFunction>)| {
        let name: String = self_.get("name")?;
        let self_key = Arc::new(lua.create_registry_value(self_.clone())?);
        let on_ready_key = on_ready.map(|f| lua.create_registry_value(f)).transpose()?.map(Arc::new);
        let cb = lua.create_function(move |lua, (result, err): (LuaValue, LuaValue)| {
            if !matches!(err, LuaValue::Nil) {
                let core: LuaTable = req(lua, "core")?;
                let warn: LuaFunction = core.get("warn")?;
                let msg = if let LuaValue::Table(ref t) = err {
                    t.get::<String>("message").unwrap_or_else(|_| format!("{name} initialize error"))
                } else { format!("{name} initialize error") };
                let _ = warn.call::<LuaValue>(format!("LSP {name} initialize failed: {msg}"));
                return Ok(());
            }
            let client: LuaTable = lua.registry_value(&self_key)?;
            let caps = if let LuaValue::Table(ref rt) = result {
                rt.get::<LuaValue>("capabilities").unwrap_or(LuaValue::Table(lua.create_table()?))
            } else { LuaValue::Table(lua.create_table()?) };
            client.set("capabilities", caps)?;
            client.set("is_initialized", true)?;
            let notify: LuaFunction = client.get("notify")?;
            notify.call::<bool>((client.clone(), "initialized", LuaValue::Table(lua.create_table()?), true))?;
            let flush: LuaFunction = client.get("flush_pre_init_queue")?;
            flush.call::<()>(client.clone())?;
            if let Some(ref key) = on_ready_key {
                let f: LuaFunction = lua.registry_value(key)?;
                let core: LuaTable = req(lua, "core")?;
                let try_fn: LuaFunction = core.get("try")?;
                let _ = try_fn.call::<LuaValue>((f, client, result));
            }
            Ok(())
        })?;
        let req_fn: LuaFunction = self_.get("request")?;
        req_fn.call::<bool>((self_, "initialize", LuaValue::Table(params), cb, true))?;
        Ok(())
    })?)?;

    class.set("shutdown", lua.create_function(|lua, self_: LuaTable| {
        if self_.get::<bool>("is_shutting_down").unwrap_or(false) { return Ok(()); }
        let tid: LuaValue = self_.get("transport_id")?;
        if matches!(tid, LuaValue::Nil) { return Ok(()); }
        let tid_val: i64 = if let LuaValue::Integer(n) = tid { n } else { return Ok(()); };
        self_.set("is_shutting_down", true)?;
        let self_key = Arc::new(lua.create_registry_value(self_.clone())?);
        let cb = lua.create_function(move |lua, _: LuaMultiValue| {
            let client: LuaTable = lua.registry_value(&self_key)?;
            let notify: LuaFunction = client.get("notify")?;
            notify.call::<bool>((client, "exit", LuaValue::Nil, true))?;
            let native: LuaTable = req(lua, "lsp_transport")?;
            native.get::<LuaFunction>("terminate")?.call::<()>(tid_val)?;
            native.get::<LuaFunction>("remove")?.call::<()>(tid_val)?;
            Ok(())
        })?;
        let req_fn: LuaFunction = self_.get("request")?;
        req_fn.call::<bool>((self_, "shutdown", LuaValue::Nil, cb, true))?;
        Ok(())
    })?)?;

    // new: factory — creates instances, starts reader coroutine
    let class_key = Arc::new(lua.create_registry_value(class.clone())?);
    class.set("new", lua.create_function(move |lua, (name, spec, root_dir, handlers): (String, LuaTable, String, LuaTable)| {
        let native: LuaTable = req(lua, "lsp_transport")?;
        let spawn: LuaFunction = native.get("spawn")?;
        let cmd: LuaValue = spec.get("command")?;
        let env: LuaValue = spec.get("env").unwrap_or(LuaValue::Nil);
        let tid: i64 = match spawn.call::<i64>((cmd, root_dir.clone(), env)) {
            Ok(n) => n,
            Err(e) => return Ok(LuaMultiValue::from_vec(vec![
                LuaValue::Nil,
                LuaValue::String(lua.create_string(e.to_string())?),
            ])),
        };
        let inst = lua.create_table()?;
        inst.set("name", name)?;
        inst.set("spec", spec)?;
        inst.set("root_dir", root_dir)?;
        inst.set("transport_id", tid)?;
        inst.set("handlers", handlers)?;
        inst.set("next_request_id", 0i64)?;
        inst.set("pending", lua.create_table()?)?;
        inst.set("pre_init_queue", lua.create_table()?)?;
        inst.set("is_initialized", false)?;
        inst.set("is_shutting_down", false)?;
        inst.set("capabilities", lua.create_table()?)?;
        let cls: LuaTable = lua.registry_value(&class_key)?;
        lua.globals().get::<LuaFunction>("setmetatable")?.call::<LuaTable>((inst.clone(), cls))?;

        // reader coroutine — polls transport, dispatches messages
        // Returns: sleep delay (f64) when should yield, or nil when done.
        // coroutine.yield cannot be called from a Rust C function (no lua_yieldk
        // continuation), so the yield lives in a thin Lua wrapper below.
        let inst_key = Arc::new(lua.create_registry_value(inst.clone())?);
        let tick = lua.create_function(move |lua, ()| -> LuaResult<LuaValue> {
            let client: LuaTable = lua.registry_value(&inst_key)?;
            let client_name: String = client.get("name")?;
            let transport_id: i64 = client.get("transport_id")?;
            let native: LuaTable = req(lua, "lsp_transport")?;
            let poll: LuaFunction = native.get("poll")?;
            let mut had_output = false;
            match poll.call::<LuaValue>((transport_id, 64i64)) {
                Ok(LuaValue::Table(polled)) => {
                    if let LuaValue::Table(msgs) = polled.get::<LuaValue>("messages")? {
                        let len = msgs.raw_len();
                        if len > 0 {
                            had_output = true;
                            let hm: LuaFunction = client.get("handle_message")?;
                            for i in 1..=len {
                                let msg: LuaTable = msgs.get(i)?;
                                hm.call::<()>((client.clone(), msg))?;
                            }
                        }
                    }
                    if let LuaValue::Table(stderr) = polled.get::<LuaValue>("stderr")? {
                        let len = stderr.raw_len();
                        if len > 0 {
                            had_output = true;
                            let core: LuaTable = req(lua, "core")?;
                            let lq: LuaFunction = core.get("log_quiet")?;
                            for i in 1..=len {
                                let line: String = stderr.get::<String>(i).unwrap_or_default();
                                let _ = lq.call::<LuaValue>(format!("LSP {client_name} stderr: {}", line.trim_end()));
                            }
                        }
                    }
                    let running: bool = polled.get("running").unwrap_or(false);
                    let has_msgs = polled.get::<LuaValue>("messages")
                        .map(|v| if let LuaValue::Table(t) = v { t.raw_len() > 0 } else { false })
                        .unwrap_or(false);
                    let has_err = polled.get::<LuaValue>("stderr")
                        .map(|v| if let LuaValue::Table(t) = v { t.raw_len() > 0 } else { false })
                        .unwrap_or(false);
                    if !running && !has_msgs && !has_err {
                        // signal done; fire on_exit then return nil to stop the loop
                        let handlers: LuaTable = client.get("handlers")?;
                        if let LuaValue::Function(on_exit) = handlers.get::<LuaValue>("on_exit")? {
                            let core: LuaTable = req(lua, "core")?;
                            let try_fn: LuaFunction = core.get("try")?;
                            let _ = try_fn.call::<LuaValue>((on_exit, client));
                        }
                        return Ok(LuaValue::Nil);
                    }
                }
                _ => {
                    // transport gone; fire on_exit and stop
                    let handlers: LuaTable = client.get("handlers")?;
                    if let LuaValue::Function(on_exit) = handlers.get::<LuaValue>("on_exit")? {
                        let core: LuaTable = req(lua, "core")?;
                        let try_fn: LuaFunction = core.get("try")?;
                        let _ = try_fn.call::<LuaValue>((on_exit, client));
                    }
                    return Ok(LuaValue::Nil);
                }
            }
            // yield if nothing arrived this tick, skip yield when busy
            if had_output { Ok(LuaValue::Number(0.0)) } else { Ok(LuaValue::Number(0.05)) }
        })?;
        // Lua wrapper: loops until tick() returns nil, yielding between iterations.
        let reader: LuaFunction = lua.load(
            "local t = ...; return function() local d = t(); while d do coroutine.yield(d); d = t() end end"
        ).call::<LuaFunction>(tick)?;
        req(lua, "core")?.get::<LuaFunction>("add_thread")?.call::<()>(reader)?;
        Ok(LuaMultiValue::from_vec(vec![LuaValue::Table(inst)]))
    })?)?;

    Ok(LuaValue::Table(class))
}

// ─── INLINE DIAGNOSTIC HELPERS ───────────────────────────────────────────────

/// Trims leading and trailing whitespace from a Lua string value.
fn trim_text(s: &str) -> &str {
    s.trim()
}

/// Returns the one-line text used for the inline end-of-line diagnostic hint.
fn inline_diagnostic_text(diagnostic: &LuaTable) -> LuaResult<Option<String>> {
    let msg: String = match diagnostic.get::<LuaValue>("message")? {
        LuaValue::String(s) => s.to_str()?.to_owned(),
        _ => return Ok(None),
    };
    let msg = msg.replace("\r\n", "\n").replace('\r', "\n");
    let first = trim_text(msg.split('\n').next().unwrap_or(""));
    if first.is_empty() {
        return Ok(None);
    }
    let collapsed: String = first.split_whitespace().collect::<Vec<_>>().join(" ");
    Ok(Some(collapsed))
}

/// Builds a tooltip header string from a diagnostic table (severity · source · code).
fn diagnostic_tooltip_text(diagnostic: &LuaTable) -> LuaResult<Option<String>> {
    let severity: i64 = diagnostic.get::<LuaValue>("severity")
        .ok()
        .and_then(|v| if let LuaValue::Integer(n) = v { Some(n) } else { None })
        .unwrap_or(3);
    let label = match severity {
        1 => "Error",
        2 => "Warning",
        3 => "Info",
        _ => "Hint",
    };
    let mut parts = vec![label.to_owned()];
    if let Ok(LuaValue::String(s)) = diagnostic.get::<LuaValue>("source") {
        let src = s.to_str()?.to_owned();
        if !src.is_empty() { parts.push(src); }
    }
    let code = diagnostic.get::<LuaValue>("code").unwrap_or(LuaValue::Nil);
    let code_str = match &code {
        LuaValue::String(s) => s.to_str()?.to_owned(),
        LuaValue::Integer(n) => n.to_string(),
        LuaValue::Number(n) => n.to_string(),
        _ => String::new(),
    };
    if !code_str.is_empty() { parts.push(code_str); }

    let msg: String = match diagnostic.get::<LuaValue>("message")? {
        LuaValue::String(s) => s.to_str()?.to_owned(),
        _ => return Ok(None),
    };
    let msg = msg.replace("\r\n", "\n").replace('\r', "\n");
    let prefix = parts.join(" \u{00B7} "); // middle dot U+00B7
    if prefix.is_empty() {
        return Ok(Some(msg));
    }
    Ok(Some(format!("{prefix}\n{msg}")))
}

/// Draws the inline end-of-line diagnostic hint for `line` in `view`.
fn draw_inline_diagnostic(lua: &Lua, view: &LuaTable, mgr: &LuaTable, line: i64) -> LuaResult<()> {
    let get_inline: LuaFunction = mgr.get("get_inline_diagnostic")?;
    let doc: LuaTable = view.get("doc")?;
    let result: LuaMultiValue = get_inline.call((doc, line))?;
    let mut it = result.into_iter();
    let diag = match it.next() {
        Some(LuaValue::Table(t)) => t,
        _ => return Ok(()),
    };
    let end_col: Option<i64> = match it.next() {
        Some(LuaValue::Integer(n)) => Some(n),
        Some(LuaValue::Number(n)) => Some(n as i64),
        _ => None,
    };
    let text = match inline_diagnostic_text(&diag)? {
        Some(t) => t,
        None => return Ok(()),
    };

    let style: LuaTable = req(lua, "core.style")?;
    let font: LuaTable = view.get::<LuaFunction>("get_font")?.call(view.clone())?;
    let get_width: LuaFunction = font.get("get_width")?;
    let text_w: f64 = get_width.call((font.clone(), text.clone()))?;
    if text_w <= 0.0 { return Ok(()); }

    let get_pos: LuaFunction = view.get("get_line_screen_position")?;
    let pos: LuaMultiValue = get_pos.call((view.clone(), line))?;
    let mut pit = pos.into_iter();
    let x: f64 = match pit.next() { Some(LuaValue::Number(n)) => n, Some(LuaValue::Integer(n)) => n as f64, _ => return Ok(()) };
    let y: f64 = match pit.next() { Some(LuaValue::Number(n)) => n, Some(LuaValue::Integer(n)) => n as f64, _ => return Ok(()) };
    let lh: f64 = view.get::<LuaFunction>("get_line_height")?.call::<f64>(view.clone())?;

    let v_scrollbar: LuaTable = view.get("v_scrollbar")?;
    let track_rect: LuaMultiValue = v_scrollbar.get::<LuaFunction>("get_track_rect")?.call(v_scrollbar)?;
    let mut tri = track_rect.into_iter();
    let _: LuaValue = tri.next().unwrap_or(LuaValue::Nil);
    let _: LuaValue = tri.next().unwrap_or(LuaValue::Nil);
    let scroll_w: f64 = match tri.next() { Some(LuaValue::Number(n)) => n, Some(LuaValue::Integer(n)) => n as f64, _ => 0.0 };

    let pos_x: f64 = view.get::<LuaValue>("position").and_then(|v| if let LuaValue::Table(t) = v { t.get::<f64>("x") } else { Ok(0.0) }).unwrap_or(0.0);
    let size_x: f64 = view.get::<LuaValue>("size").and_then(|v| if let LuaValue::Table(t) = v { t.get::<f64>("x") } else { Ok(0.0) }).unwrap_or(0.0);
    let gutter_w: f64 = view.get::<LuaFunction>("get_gutter_width")?.call::<f64>(view.clone())?;
    let clip_left = pos_x + gutter_w;
    let clip_right = pos_x + size_x - scroll_w;

    let padding_x: f64 = style.get::<LuaValue>("padding")
        .ok()
        .and_then(|v| if let LuaValue::Table(t) = v { t.get::<f64>("x").ok() } else { None })
        .unwrap_or(4.0);
    let font_space_w: f64 = get_width.call((font.clone(), " "))?;
    let inline_diagnostic_side_padding = f64::max(padding_x, font_space_w.floor());
    let space2_w: f64 = get_width.call((font.clone(), "  "))?;
    let inline_diagnostic_gap = space2_w.floor();

    let max_x = clip_right - inline_diagnostic_side_padding - text_w;
    if max_x <= clip_left { return Ok(()); }

    let line_text: String = {
        let lines_tbl: LuaTable = view.get::<LuaTable>("doc")?.get("lines")?;
        lines_tbl.get::<LuaValue>(line).ok()
            .and_then(|v| if let LuaValue::String(s) = v { s.to_str().ok().map(|s| s.to_owned()) } else { None })
            .unwrap_or_else(|| "\n".to_owned())
    };
    let line_len = line_text.len() as i64;
    let anchor_col = i64::max(1, i64::min(end_col.unwrap_or(line_len + 1) + 1, line_len + 1));
    let col_x: LuaMultiValue = view.get::<LuaFunction>("get_line_screen_position")?
        .call((view.clone(), line, anchor_col))?;
    let anchor_x: f64 = match col_x.into_iter().next() {
        Some(LuaValue::Number(n)) => n,
        Some(LuaValue::Integer(n)) => n as f64,
        _ => x,
    };
    let anchor_x = anchor_x + inline_diagnostic_gap;
    let text_x = f64::max(anchor_x, max_x);
    if text_x + text_w > clip_right - inline_diagnostic_side_padding { return Ok(()); }

    let severity: i64 = diag.get::<LuaValue>("severity")
        .ok()
        .and_then(|v| if let LuaValue::Integer(n) = v { Some(n) } else { None })
        .unwrap_or(3);

    let renderer: LuaTable = lua.globals().get("renderer")?;
    let draw_rect: LuaFunction = renderer.get("draw_rect")?;
    let bg: LuaValue = style.get("background")?;
    draw_rect.call::<()>((
        (text_x - inline_diagnostic_side_padding) as i64,
        y as i64, (text_w + inline_diagnostic_side_padding * 2.0) as i64, lh as i64,
        bg,
    ))?;

    let color = mgr_diagnostic_color(lua, severity)?;
    let common: LuaTable = req(lua, "core.common")?;
    let draw_text: LuaFunction = common.get("draw_text")?;
    let text_y_off: f64 = view.get::<LuaFunction>("get_line_text_y_offset")?.call::<f64>(view.clone())?;
    let font_h: f64 = font.get::<LuaFunction>("get_height")?.call::<f64>(font.clone())?;
    draw_text.call::<()>((font, color, text, LuaValue::Nil, text_x as i64, (y + text_y_off) as i64, text_w as i64, font_h as i64))?;
    Ok(())
}

// ─── INIT MODULE (plugins.lsp) ────────────────────────────────────────────────

/// Pure-Rust replacement for INIT_SOURCE — wires config, patches DocView/Doc/RootView, adds keymap.
fn init_lsp_plugin(lua: &Lua) -> LuaResult<LuaValue> {
    // Config defaults
    let common: LuaTable = req(lua, "core.common")?;
    let config: LuaTable = req(lua, "core.config")?;
    let plugins_cfg: LuaTable = config.get("plugins")?;

    let config_spec = lua.create_table()?;
    config_spec.set("name", "LSP")?;
    {
        let item = lua.create_table()?;
        item.set("label", "Load On Startup")?;
        item.set("description", "Load the LSP plugin during editor startup.")?;
        item.set("path", "load_on_startup")?;
        item.set("type", "toggle")?;
        item.set("default", true)?;
        config_spec.set(1, item)?;
    }
    {
        let item = lua.create_table()?;
        item.set("label", "Semantic Highlighting")?;
        item.set("description", "Apply semantic token overlays from LSP servers.")?;
        item.set("path", "semantic_highlighting")?;
        item.set("type", "toggle")?;
        item.set("default", true)?;
        config_spec.set(2, item)?;
    }
    {
        let item = lua.create_table()?;
        item.set("label", "Inline Diagnostics")?;
        item.set("description", "Render LSP diagnostics in the editor gutter and text area.")?;
        item.set("path", "inline_diagnostics")?;
        item.set("type", "toggle")?;
        item.set("default", true)?;
        config_spec.set(3, item)?;
    }
    {
        let item = lua.create_table()?;
        item.set("label", "Format On Save")?;
        item.set("description", "Run document formatting before saving when the server supports it.")?;
        item.set("path", "format_on_save")?;
        item.set("type", "toggle")?;
        item.set("default", true)?;
        config_spec.set(4, item)?;
    }

    // Read legacy flat config.lsp for backwards compat defaults
    let legacy: LuaValue = config.get("lsp").unwrap_or(LuaValue::Nil);
    let legacy_bool = |key: &str, fallback: bool| -> bool {
        if let LuaValue::Table(ref t) = legacy {
            match t.get::<LuaValue>(key) {
                Ok(LuaValue::Boolean(b)) => b,
                Ok(LuaValue::Nil) => fallback,
                _ => fallback,
            }
        } else { fallback }
    };

    let defaults = lua.create_table()?;
    defaults.set("config_spec", config_spec)?;
    defaults.set("load_on_startup", legacy_bool("load_on_startup", true))?;
    defaults.set("semantic_highlighting", legacy_bool("semantic_highlighting", true))?;
    defaults.set("inline_diagnostics", legacy_bool("inline_diagnostics", true))?;
    defaults.set("format_on_save", legacy_bool("format_on_save", true))?;

    let existing: LuaValue = plugins_cfg.get("lsp").unwrap_or(LuaValue::Nil);
    let merged: LuaTable = common.get::<LuaFunction>("merge")?.call((defaults, existing))?;
    plugins_cfg.set("lsp", merged)?;

    // Load manager
    let manager: LuaTable = req(lua, "plugins.lsp.server-manager")?;
    manager.get::<LuaFunction>("reload_config")?.call::<()>(())?;
    manager.get::<LuaFunction>("start_semantic_refresh_loop")?.call::<()>(())?;

    let mgr_key = Arc::new(lua.create_registry_value(manager.clone())?);

    // Patch core.open_doc
    {
        let mk = Arc::clone(&mgr_key);
        let core: LuaTable = req(lua, "core")?;
        let old_open_doc: LuaFunction = core.get("open_doc")?;
        let old_key = Arc::new(lua.create_registry_value(old_open_doc)?);
        core.set("open_doc", lua.create_function(move |lua, args: LuaMultiValue| {
            let old: LuaFunction = lua.registry_value(&old_key)?;
            let result: LuaMultiValue = old.call(args)?;
            let doc = result.iter().next().cloned().unwrap_or(LuaValue::Nil);
            if let LuaValue::Table(ref doc_t) = doc {
                let has_abs = !matches!(doc_t.get::<LuaValue>("abs_filename")?, LuaValue::Nil);
                let large = doc_t.get::<bool>("large_file_mode").unwrap_or(false);
                if has_abs && !large {
                    let m: LuaTable = lua.registry_value(&mk)?;
                    m.get::<LuaFunction>("open_doc")?.call::<()>(doc_t.clone())?;
                }
            }
            Ok(result)
        })?)?;
    }

    // Patch Doc.on_text_change
    {
        let mk = Arc::clone(&mgr_key);
        let doc_class: LuaTable = req(lua, "core.doc")?;
        let old_fn: LuaFunction = doc_class.get("on_text_change")?;
        let old_key = Arc::new(lua.create_registry_value(old_fn)?);
        doc_class.set("on_text_change", lua.create_function(move |lua, (self_, change_type): (LuaTable, LuaValue)| {
            let old: LuaFunction = lua.registry_value(&old_key)?;
            old.call::<()>((self_.clone(), change_type))?;
            let has_abs = !matches!(self_.get::<LuaValue>("abs_filename")?, LuaValue::Nil);
            let large = self_.get::<bool>("large_file_mode").unwrap_or(false);
            if has_abs && !large {
                let m: LuaTable = lua.registry_value(&mk)?;
                m.get::<LuaFunction>("on_doc_change")?.call::<()>(self_)?;
            }
            Ok(())
        })?)?;
    }

    // Patch RootView.on_text_input
    {
        let mk = Arc::clone(&mgr_key);
        let rootview: LuaTable = req(lua, "core.rootview")?;
        let old_fn: LuaFunction = rootview.get("on_text_input")?;
        let old_key = Arc::new(lua.create_registry_value(old_fn)?);
        rootview.set("on_text_input", lua.create_function(move |lua, args: LuaMultiValue| {
            let old: LuaFunction = lua.registry_value(&old_key)?;
            let text = args.iter().nth(1).cloned().unwrap_or(LuaValue::Nil);
            old.call::<()>(args)?;
            let m: LuaTable = lua.registry_value(&mk)?;
            m.get::<LuaFunction>("maybe_trigger_completion")?.call::<()>(text.clone())?;
            m.get::<LuaFunction>("maybe_trigger_signature_help")?.call::<()>(text)?;
            Ok(())
        })?)?;
    }

    // Patch DocView.draw_line_gutter
    {
        let mk = Arc::clone(&mgr_key);
        let docview: LuaTable = req(lua, "core.docview")?;
        let old_fn: LuaFunction = docview.get("draw_line_gutter")?;
        let old_key = Arc::new(lua.create_registry_value(old_fn)?);
        docview.set("draw_line_gutter", lua.create_function(move |lua, (self_, line, x, y, width): (LuaTable, i64, LuaValue, LuaValue, LuaValue)| {
            let old: LuaFunction = lua.registry_value(&old_key)?;
            let lh: LuaValue = old.call((self_.clone(), line, x.clone(), y.clone(), width))?;
            let config_plugins: LuaTable = req(lua, "core.config")?.get("plugins")?;
            let lsp_cfg: LuaTable = config_plugins.get("lsp")?;
            let inline_on = lsp_cfg.get::<LuaValue>("inline_diagnostics")
                .map(|v| !matches!(v, LuaValue::Boolean(false))).unwrap_or(true);
            let doc: LuaTable = self_.get("doc")?;
            let has_abs = !matches!(doc.get::<LuaValue>("abs_filename")?, LuaValue::Nil);
            let large = doc.get::<bool>("large_file_mode").unwrap_or(false);
            if !inline_on || !has_abs || large {
                return Ok(lh);
            }
            let m: LuaTable = lua.registry_value(&mk)?;
            let sev_val: LuaValue = m.get::<LuaFunction>("get_line_diagnostic_severity")?.call((doc.clone(), line))?;
            let severity = match sev_val {
                LuaValue::Integer(n) => n,
                LuaValue::Number(n) => n as i64,
                _ => return Ok(lh),
            };
            {
                let line_h: f64 = self_.call_method::<f64>("get_line_height", ())?;
                let marker_size = f64::max(4.0, (line_h * 0.22).floor()) as i64;
                let x_val: f64 = match x { LuaValue::Number(n) => n, LuaValue::Integer(n) => n as f64, _ => 0.0 };
                let y_val: f64 = match y { LuaValue::Number(n) => n, LuaValue::Integer(n) => n as f64, _ => 0.0 };
                let style: LuaTable = req(lua, "core.style")?;
                let padding_x: f64 = style.get::<LuaTable>("padding")?.get::<f64>("x")?;
                let marker_x = (x_val + f64::max(2.0, padding_x - marker_size as f64 - 2.0)) as i64;
                let marker_y = (y_val + ((line_h - marker_size as f64) / 2.0).floor()) as i64;
                let renderer: LuaTable = lua.globals().get("renderer")?;
                let draw_rect: LuaFunction = renderer.get("draw_rect")?;
                let color = mgr_diagnostic_color(lua, severity)?;
                draw_rect.call::<()>((marker_x, marker_y, marker_size, marker_size, color))?;
                // accent dot on current line
                let sel: LuaMultiValue = doc.get::<LuaFunction>("get_selection")?.call(doc)?;
                let current_line = match sel.into_iter().next() { Some(LuaValue::Integer(n)) => n, _ => 0 };
                if line == current_line {
                    let accent: LuaValue = style.get("accent")?;
                    draw_rect.call::<()>((marker_x + marker_size + 2, marker_y, marker_size, marker_size, accent))?;
                }
            }
            Ok(lh)
        })?)?;
    }

    // Patch DocView.on_mouse_pressed
    {
        let mk = Arc::clone(&mgr_key);
        let docview: LuaTable = req(lua, "core.docview")?;
        let old_fn: LuaFunction = docview.get("on_mouse_pressed")?;
        let old_key = Arc::new(lua.create_registry_value(old_fn)?);
        docview.set("on_mouse_pressed", lua.create_function(move |lua, (self_, button, x, y, clicks): (LuaTable, String, f64, f64, i64)| {
            if button == "left" {
                let hovering_gutter = self_.get::<bool>("hovering_gutter").unwrap_or(false);
                let doc: LuaTable = self_.get("doc")?;
                let large = doc.get::<bool>("large_file_mode").unwrap_or(false);
                if hovering_gutter && !large {
                    let line: i64 = self_.call_method::<i64>("resolve_screen_position", (x as i64, y as i64))?;
                    let m: LuaTable = lua.registry_value(&mk)?;
                    let sev_val: LuaValue = m.get::<LuaFunction>("get_line_diagnostic_severity")?.call((doc, line))?;
                    if !matches!(sev_val, LuaValue::Nil) {
                        let line_h: f64 = self_.call_method::<f64>("get_line_height", ())?;
                        let marker_size = f64::max(4.0, (line_h * 0.22).floor());
                        let style: LuaTable = req(lua, "core.style")?;
                        let padding_x: f64 = style.get::<LuaTable>("padding")?.get::<f64>("x")?;
                        let pos_x: f64 = self_.get::<LuaTable>("position")?.get::<f64>("x")?;
                        let marker_x = pos_x + f64::max(2.0, padding_x - marker_size - 2.0);
                        if x >= marker_x && x <= marker_x + marker_size * 2.0 + 4.0 {
                            m.get::<LuaFunction>("quick_fix_for_line")?.call::<()>(line)?;
                            return Ok(LuaValue::Boolean(true));
                        }
                    }
                }
            }
            let old: LuaFunction = lua.registry_value(&old_key)?;
            old.call((self_, button, x as i64, y as i64, clicks))
        })?)?;
    }

    // Patch DocView.draw_overlay
    {
        let mk = Arc::clone(&mgr_key);
        let docview: LuaTable = req(lua, "core.docview")?;
        let old_fn: LuaFunction = docview.get("draw_overlay")?;
        let old_key = Arc::new(lua.create_registry_value(old_fn)?);
        docview.set("draw_overlay", lua.create_function(move |lua, self_: LuaTable| {
            let old: LuaFunction = lua.registry_value(&old_key)?;
            old.call::<()>(self_.clone())?;
            let config_plugins: LuaTable = req(lua, "core.config")?.get("plugins")?;
            let lsp_cfg: LuaTable = config_plugins.get("lsp")?;
            let inline_on = lsp_cfg.get::<LuaValue>("inline_diagnostics")
                .map(|v| !matches!(v, LuaValue::Boolean(false))).unwrap_or(true);
            let doc: LuaTable = self_.get("doc")?;
            let has_abs = !matches!(doc.get::<LuaValue>("abs_filename")?, LuaValue::Nil);
            let large = doc.get::<bool>("large_file_mode").unwrap_or(false);
            if !inline_on || !has_abs || large { return Ok(()); }

            let m: LuaTable = lua.registry_value(&mk)?;
            let range: LuaMultiValue = self_.call_method::<LuaMultiValue>("get_visible_line_range", ())?;
            let mut rit = range.into_iter();
            let minline: i64 = match rit.next() { Some(LuaValue::Integer(n)) => n, _ => 1 };
            let maxline: i64 = match rit.next() { Some(LuaValue::Integer(n)) => n, _ => minline };
            let style: LuaTable = req(lua, "core.style")?;
            let caret_width: i64 = style.get::<LuaValue>("caret_width")
                .map(|v| match v { LuaValue::Integer(n) => n, LuaValue::Number(n) => n as i64, _ => 1 })
                .unwrap_or(1);
            let line_size = i64::max(1, caret_width);
            let renderer: LuaTable = lua.globals().get("renderer")?;
            let draw_rect: LuaFunction = renderer.get("draw_rect")?;

            for line in minline..=maxline {
                let segs_val: LuaValue = m.get::<LuaFunction>("get_line_diagnostic_segments")?.call((doc.clone(), line))?;
                if let LuaValue::Table(segments) = segs_val {
                    let pos: LuaMultiValue = self_.call_method::<LuaMultiValue>("get_line_screen_position", line)?;
                    let mut pit = pos.into_iter();
                    let _line_x: f64 = match pit.next() { Some(LuaValue::Number(n)) => n, Some(LuaValue::Integer(n)) => n as f64, _ => 0.0 };
                    let line_y: f64 = match pit.next() { Some(LuaValue::Number(n)) => n, Some(LuaValue::Integer(n)) => n as f64, _ => 0.0 };
                    let lh: f64 = self_.call_method::<f64>("get_line_height", ())?;
                    for seg in segments.sequence_values::<LuaTable>() {
                        let seg = seg?;
                        let col1: i64 = seg.get("col1")?;
                        let col2: i64 = seg.get("col2")?;
                        let severity: i64 = seg.get::<LuaValue>("severity")
                            .map(|v| match v { LuaValue::Integer(n) => n, _ => 3 }).unwrap_or(3);
                        let pos1: LuaMultiValue = self_.call_method::<LuaMultiValue>("get_line_screen_position", (line, col1))?;
                        let start_x: f64 = match pos1.into_iter().next() { Some(LuaValue::Number(n)) => n, Some(LuaValue::Integer(n)) => n as f64, _ => 0.0 };
                        let pos2: LuaMultiValue = self_.call_method::<LuaMultiValue>("get_line_screen_position", (line, col2))?;
                        let end_x: f64 = match pos2.into_iter().next() { Some(LuaValue::Number(n)) => n, Some(LuaValue::Integer(n)) => n as f64, _ => 0.0 };
                        let width = f64::max((end_x - start_x).abs(), f64::max(2.0, caret_width as f64 * 2.0));
                        let color = mgr_diagnostic_color(lua, severity)?;
                        draw_rect.call::<()>((
                            f64::min(start_x, end_x) as i64,
                            (line_y + lh - line_size as f64) as i64,
                            width as i64,
                            line_size,
                            color,
                        ))?;
                    }
                }
                draw_inline_diagnostic(lua, &self_, &m, line)?;
            }

            // Tooltip deferred draw
            let tooltip: LuaValue = self_.get("lsp_diagnostic_tooltip")?;
            if let LuaValue::Table(ref tt) = tooltip {
                let has_text = !matches!(tt.get::<LuaValue>("text")?, LuaValue::Nil);
                let alpha: f64 = tt.get::<LuaValue>("alpha")
                    .map(|v| match v { LuaValue::Number(n) => n, LuaValue::Integer(n) => n as f64, _ => 0.0 })
                    .unwrap_or(0.0);
                if has_text && alpha > 0.0 {
                    let core: LuaTable = req(lua, "core")?;
                    let root_view: LuaTable = core.get("root_view")?;
                    let self_c = self_.clone();
                    let draw_tt = lua.create_function(move |_lua, _view: LuaValue| {
                        self_c.call_method::<()>("draw_lsp_diagnostic_tooltip", ())
                    })?;
                    root_view.call_method::<()>("defer_draw", (draw_tt, self_))?;
                }
            }
            Ok(())
        })?)?;
    }

    // Add DocView.update_lsp_diagnostic_tooltip method
    {
        let mk = Arc::clone(&mgr_key);
        let docview: LuaTable = req(lua, "core.docview")?;
        docview.set("update_lsp_diagnostic_tooltip", lua.create_function(move |lua, (self_, x, y): (LuaTable, f64, f64)| {
            let config_plugins: LuaTable = req(lua, "core.config")?.get("plugins")?;
            let lsp_cfg: LuaTable = config_plugins.get("lsp")?;
            let inline_on = lsp_cfg.get::<LuaValue>("inline_diagnostics")
                .map(|v| !matches!(v, LuaValue::Boolean(false))).unwrap_or(true);
            let doc: LuaTable = self_.get("doc")?;
            let has_abs = !matches!(doc.get::<LuaValue>("abs_filename")?, LuaValue::Nil);
            let large = doc.get::<bool>("large_file_mode").unwrap_or(false);
            if !inline_on || !has_abs || large {
                self_.set("lsp_diagnostic_tooltip", LuaValue::Nil)?;
                return Ok(());
            }

            let existing_tooltip: LuaValue = self_.get("lsp_diagnostic_tooltip")?;
            let tooltip = match existing_tooltip {
                LuaValue::Table(t) => t,
                _ => {
                    let t = lua.create_table()?;
                    t.set("x", 0i64)?; t.set("y", 0i64)?;
                    t.set("begin", 0.0f64)?; t.set("alpha", 0i64)?;
                    t
                }
            };

            let m: LuaTable = lua.registry_value(&mk)?;
            let line_col: LuaMultiValue = self_.call_method::<LuaMultiValue>("resolve_screen_position", (x as i64, y as i64))?;
            let mut lc = line_col.into_iter();
            let line: i64 = match lc.next() { Some(LuaValue::Integer(n)) => n, _ => return Ok(()) };
            let col: LuaValue = lc.next().unwrap_or(LuaValue::Nil);

            let hovering_gutter = self_.get::<bool>("hovering_gutter").unwrap_or(false);
            let diag_val: LuaValue = if hovering_gutter {
                m.get::<LuaFunction>("get_hover_diagnostic")?.call((doc, line, LuaValue::Nil))?
            } else {
                m.get::<LuaFunction>("get_hover_diagnostic")?.call((doc, line, col))?
            };

            let diag = match diag_val {
                LuaValue::Table(t) => t,
                _ => {
                    self_.set("lsp_diagnostic_tooltip", LuaValue::Nil)?;
                    return Ok(());
                }
            };

            let text = match diagnostic_tooltip_text(&diag)? {
                Some(t) => t,
                None => {
                    self_.set("lsp_diagnostic_tooltip", LuaValue::Nil)?;
                    return Ok(());
                }
            };

            let style: LuaTable = req(lua, "core.style")?;
            let diagnostic_tooltip_max_width: i64 = {
                let scale: f64 = lua.globals().get::<LuaValue>("SCALE")
                    .map(|v| match v { LuaValue::Number(n) => n, LuaValue::Integer(n) => n as f64, _ => 1.0 })
                    .unwrap_or(1.0);
                (420.0 * scale).floor() as i64
            };

            let existing_text: LuaValue = tooltip.get("text")?;
            let same_text = match &existing_text {
                LuaValue::String(s) => s.to_str().ok().map(|s| s == text).unwrap_or(false),
                _ => false,
            };
            if !same_text {
                tooltip.set("text", text.clone())?;
                let font: LuaTable = style.get("font")?;
                let padding_x: f64 = style.get::<LuaTable>("padding")?.get::<f64>("x")?;
                let lines = mgr_wrap_tooltip_lines(lua, &font, &text, diagnostic_tooltip_max_width as f64 - padding_x * 2.0)?;
                tooltip.set("lines", lines)?;
                let sys: LuaTable = lua.globals().get("system")?;
                let now: f64 = sys.get::<LuaFunction>("get_time")?.call(())?;
                tooltip.set("begin", now)?;
                tooltip.set("alpha", 0i64)?;
            }
            tooltip.set("x", x as i64)?;
            tooltip.set("y", y as i64)?;
            self_.set("lsp_diagnostic_tooltip", tooltip.clone())?;

            let diagnostic_tooltip_delay = 0.18f64;
            let sys: LuaTable = lua.globals().get("system")?;
            let now: f64 = sys.get::<LuaFunction>("get_time")?.call(())?;
            let begin: f64 = tooltip.get::<f64>("begin").unwrap_or(now);
            if now - begin > diagnostic_tooltip_delay {
                self_.call_method::<()>("move_towards", (tooltip.clone(), "alpha", 255i64, 1i64, "lsp_diagnostic_tooltip"))?;
            } else {
                tooltip.set("alpha", 0i64)?;
            }
            Ok(())
        })?)?;
    }

    // Add DocView.draw_lsp_diagnostic_tooltip method
    {
        let docview: LuaTable = req(lua, "core.docview")?;
        docview.set("draw_lsp_diagnostic_tooltip", lua.create_function(|lua, self_: LuaTable| {
            let tooltip: LuaValue = self_.get("lsp_diagnostic_tooltip")?;
            let tt = match tooltip { LuaValue::Table(t) => t, _ => return Ok(()) };
            let has_text = !matches!(tt.get::<LuaValue>("text")?, LuaValue::Nil);
            let alpha: f64 = tt.get::<LuaValue>("alpha")
                .map(|v| match v { LuaValue::Number(n) => n, LuaValue::Integer(n) => n as f64, _ => 0.0 })
                .unwrap_or(0.0);
            if !has_text || alpha <= 0.0 { return Ok(()); }

            let style: LuaTable = req(lua, "core.style")?;
            let font: LuaTable = style.get("font")?;
            let line_height: f64 = font.get::<LuaFunction>("get_height")?.call::<f64>(font.clone())?;
            let lines: LuaTable = match tt.get::<LuaValue>("lines")? {
                LuaValue::Table(t) => t,
                _ => {
                    let t = lua.create_table()?;
                    t.set(1, tt.get::<LuaValue>("text")?)?;
                    t
                }
            };
            let n_lines = lines.raw_len();
            let get_width: LuaFunction = font.get("get_width")?;
            let mut text_w: f64 = 0.0;
            for i in 1..=n_lines {
                let line_text: String = lines.get::<LuaValue>(i)
                    .ok()
                    .and_then(|v| if let LuaValue::String(s) = v { s.to_str().ok().map(|s| s.to_owned()) } else { None })
                    .unwrap_or_default();
                let w: f64 = get_width.call((font.clone(), line_text))?;
                text_w = f64::max(text_w, w);
            }

            let padding_x: f64 = style.get::<LuaTable>("padding")?.get::<f64>("x")?;
            let padding_y: f64 = style.get::<LuaTable>("padding")?.get::<f64>("y")?;
            let scale: f64 = lua.globals().get::<LuaValue>("SCALE")
                .map(|v| match v { LuaValue::Number(n) => n, LuaValue::Integer(n) => n as f64, _ => 1.0 })
                .unwrap_or(1.0);
            let diagnostic_tooltip_max_width = (420.0 * scale).floor();
            let diagnostic_tooltip_border = 1i64;
            let diagnostic_tooltip_offset = line_height;

            let w = f64::min(diagnostic_tooltip_max_width, text_w + padding_x * 2.0);
            let h = f64::max(line_height, n_lines as f64 * line_height) + padding_y * 2.0;
            let tt_x: f64 = tt.get::<LuaValue>("x").map(|v| match v { LuaValue::Number(n) => n, LuaValue::Integer(n) => n as f64, _ => 0.0 }).unwrap_or(0.0);
            let tt_y: f64 = tt.get::<LuaValue>("y").map(|v| match v { LuaValue::Number(n) => n, LuaValue::Integer(n) => n as f64, _ => 0.0 }).unwrap_or(0.0);
            let mut x = tt_x + diagnostic_tooltip_offset;
            let mut y = tt_y + diagnostic_tooltip_offset;

            let core: LuaTable = req(lua, "core")?;
            let root_view: LuaTable = core.get("root_view")?;
            let root_node: LuaTable = root_view.get("root_node")?;
            let root_size: LuaTable = root_node.get("size")?;
            let root_w: f64 = root_size.get::<f64>("x")?;
            let root_h: f64 = root_size.get::<f64>("y")?;

            if x + w > root_w - padding_x { x = tt_x - w - diagnostic_tooltip_offset; }
            if x < padding_x { x = padding_x; }
            if y + h > root_h - padding_y { y = tt_y - h - diagnostic_tooltip_offset; }
            if y < padding_y { y = padding_y; }

            let alpha_i = alpha as u8;
            let border_color: LuaValue = {
                let text_color: LuaTable = style.get("text")?;
                let t = lua.create_table()?;
                t.set(1, text_color.get::<i64>(1)?)?;
                t.set(2, text_color.get::<i64>(2)?)?;
                t.set(3, text_color.get::<i64>(3)?)?;
                t.set(4, alpha_i as i64)?;
                LuaValue::Table(t)
            };
            let bg_color: LuaValue = {
                let bg2: LuaTable = style.get("background2")?;
                let t = lua.create_table()?;
                t.set(1, bg2.get::<i64>(1)?)?;
                t.set(2, bg2.get::<i64>(2)?)?;
                t.set(3, bg2.get::<i64>(3)?)?;
                t.set(4, alpha_i as i64)?;
                LuaValue::Table(t)
            };
            let text_color: LuaValue = {
                let tc: LuaTable = style.get("text")?;
                let t = lua.create_table()?;
                t.set(1, tc.get::<i64>(1)?)?;
                t.set(2, tc.get::<i64>(2)?)?;
                t.set(3, tc.get::<i64>(3)?)?;
                t.set(4, alpha_i as i64)?;
                LuaValue::Table(t)
            };

            let renderer: LuaTable = lua.globals().get("renderer")?;
            let draw_rect: LuaFunction = renderer.get("draw_rect")?;
            draw_rect.call::<()>((
                (x - diagnostic_tooltip_border as f64) as i64,
                (y - diagnostic_tooltip_border as f64) as i64,
                (w + diagnostic_tooltip_border as f64 * 2.0) as i64,
                (h + diagnostic_tooltip_border as f64 * 2.0) as i64,
                border_color,
            ))?;
            draw_rect.call::<()>((x as i64, y as i64, w as i64, h as i64, bg_color))?;

            let common: LuaTable = req(lua, "core.common")?;
            let draw_text: LuaFunction = common.get("draw_text")?;
            for i in 1..=n_lines {
                let line_text: String = lines.get::<LuaValue>(i)
                    .ok()
                    .and_then(|v| if let LuaValue::String(s) = v { s.to_str().ok().map(|s| s.to_owned()) } else { None })
                    .unwrap_or_default();
                draw_text.call::<()>((
                    font.clone(),
                    text_color.clone(),
                    line_text,
                    LuaValue::Nil,
                    (x + padding_x) as i64,
                    (y + padding_y + (i as f64 - 1.0) * line_height) as i64,
                    (w - padding_x * 2.0) as i64,
                    line_height as i64,
                ))?;
            }
            Ok(())
        })?)?;
    }

    // Patch DocView.on_mouse_moved
    {
        let docview: LuaTable = req(lua, "core.docview")?;
        let old_fn: LuaFunction = docview.get("on_mouse_moved")?;
        let old_key = Arc::new(lua.create_registry_value(old_fn)?);
        docview.set("on_mouse_moved", lua.create_function(move |lua, (self_, x, y, dx, dy): (LuaTable, f64, f64, f64, f64)| {
            let old: LuaFunction = lua.registry_value(&old_key)?;
            old.call::<()>((self_.clone(), x as i64, y as i64, dx as i64, dy as i64))?;
            self_.call_method::<()>("update_lsp_diagnostic_tooltip", (x as i64, y as i64))
        })?)?;
    }

    // Patch DocView.on_mouse_left
    {
        let docview: LuaTable = req(lua, "core.docview")?;
        let old_fn: LuaFunction = docview.get("on_mouse_left")?;
        let old_key = Arc::new(lua.create_registry_value(old_fn)?);
        docview.set("on_mouse_left", lua.create_function(move |lua, self_: LuaTable| {
            self_.set("lsp_diagnostic_tooltip", LuaValue::Nil)?;
            let old: LuaFunction = lua.registry_value(&old_key)?;
            old.call::<()>(self_)
        })?)?;
    }

    // Patch Doc.on_close
    {
        let mk = Arc::clone(&mgr_key);
        let doc_class: LuaTable = req(lua, "core.doc")?;
        let old_fn: LuaFunction = doc_class.get("on_close")?;
        let old_key = Arc::new(lua.create_registry_value(old_fn)?);
        doc_class.set("on_close", lua.create_function(move |lua, self_: LuaTable| {
            let large = self_.get::<bool>("large_file_mode").unwrap_or(false);
            if !large {
                let m: LuaTable = lua.registry_value(&mk)?;
                m.get::<LuaFunction>("on_doc_close")?.call::<()>(self_.clone())?;
            }
            let old: LuaFunction = lua.registry_value(&old_key)?;
            old.call::<()>(self_)
        })?)?;
    }

    // Patch Doc.save — format-on-save support
    {
        let mk = Arc::clone(&mgr_key);
        let doc_class: LuaTable = req(lua, "core.doc")?;
        let old_fn: LuaFunction = doc_class.get("save")?;
        let old_key = Arc::new(lua.create_registry_value(old_fn)?);
        doc_class.set("save", lua.create_function(move |lua, args: LuaMultiValue| {
            let self_: LuaTable = match args.iter().next() {
                Some(LuaValue::Table(t)) => t.clone(),
                _ => return Err(LuaError::runtime("Doc.save: expected self")),
            };
            let config_plugins: LuaTable = req(lua, "core.config")?.get("plugins")?;
            let lsp_cfg: LuaTable = config_plugins.get("lsp")?;
            let format_on_save = lsp_cfg.get::<LuaValue>("format_on_save")
                .map(|v| !matches!(v, LuaValue::Boolean(false))).unwrap_or(true);
            let large = self_.get::<bool>("large_file_mode").unwrap_or(false);
            let formatting = self_.get::<bool>("_formatting_before_save").unwrap_or(false);
            let has_abs = !matches!(self_.get::<LuaValue>("abs_filename")?, LuaValue::Nil);

            if format_on_save && !large && !formatting && has_abs {
                self_.set("_formatting_before_save", true)?;
                let mk2 = Arc::clone(&mk);
                let old_key2 = Arc::clone(&old_key);
                let self_c = self_.clone();
                let args_rest: Vec<LuaValue> = args.into_iter().skip(1).collect();
                let args_rest_key = Arc::new(lua.create_registry_value(lua.create_sequence_from(args_rest)?)?);
                let cb = lua.create_function(move |lua, ()| {
                    let self_r = self_c.clone();
                    let rest: LuaTable = lua.registry_value(&args_rest_key)?;
                    let mut call_args = vec![LuaValue::Table(self_r.clone())];
                    for v in rest.sequence_values::<LuaValue>() { call_args.push(v?); }
                    let old: LuaFunction = lua.registry_value(&old_key2)?;
                    let _: LuaMultiValue = old.call(LuaMultiValue::from_vec(call_args))?;
                    let large2 = self_r.get::<bool>("large_file_mode").unwrap_or(false);
                    if !large2 {
                        let m: LuaTable = lua.registry_value(&mk2)?;
                        let _ = m.get::<LuaFunction>("on_doc_save")?.call::<()>(self_r.clone());
                    }
                    self_r.set("_formatting_before_save", false)?;
                    Ok(())
                })?;
                let m: LuaTable = lua.registry_value(&mk)?;
                m.get::<LuaFunction>("format_document_for")?.call::<()>((self_.clone(), cb))?;
                return Ok(LuaMultiValue::new());
            }

            let old: LuaFunction = lua.registry_value(&old_key)?;
            let result: LuaMultiValue = old.call(args)?;
            if !large {
                let m: LuaTable = lua.registry_value(&mk)?;
                let _ = m.get::<LuaFunction>("on_doc_save")?.call::<()>(self_);
            }
            Ok(result)
        })?)?;
    }

    // Open already-loaded docs
    {
        let mk = Arc::clone(&mgr_key);
        let core: LuaTable = req(lua, "core")?;
        let docs: LuaTable = core.get("docs")?;
        let m: LuaTable = lua.registry_value(&mk)?;
        for doc in docs.sequence_values::<LuaTable>() {
            let doc = doc?;
            let has_abs = !matches!(doc.get::<LuaValue>("abs_filename")?, LuaValue::Nil);
            let large = doc.get::<bool>("large_file_mode").unwrap_or(false);
            if has_abs && !large {
                m.get::<LuaFunction>("open_doc")?.call::<()>(doc)?;
            }
        }
    }

    // Status bar item
    {
        let mk = Arc::clone(&mgr_key);
        let core: LuaTable = req(lua, "core")?;
        let sv: LuaTable = core.get("status_view")?;
        let sv_item_cls: LuaTable = sv.get("Item")?;
        let add_item: LuaFunction = sv.get("add_item")?;

        let pred = lua.create_function(|lua, ()| -> LuaResult<bool> {
            let core: LuaTable = req(lua, "core")?;
            let active: LuaValue = core.get("active_view")?;
            let view = match active { LuaValue::Table(t) => t, _ => return Ok(false) };
            let docview: LuaTable = req(lua, "core.docview")?;
            let is_dv: bool = view.get::<LuaFunction>("is")?.call((view.clone(), docview))?;
            if !is_dv { return Ok(false); }
            let doc: LuaValue = view.get("doc")?;
            let doc_t = match doc { LuaValue::Table(t) => t, _ => return Ok(false) };
            let has_abs = !matches!(doc_t.get::<LuaValue>("abs_filename")?, LuaValue::Nil);
            let large = doc_t.get::<bool>("large_file_mode").unwrap_or(false);
            Ok(has_abs && !large)
        })?;

        let get_item = lua.create_function(move |lua, ()| -> LuaResult<LuaValue> {
            let core: LuaTable = req(lua, "core")?;
            let active: LuaValue = core.get("active_view")?;
            let view = match active { LuaValue::Table(t) => t, _ => return Ok(LuaValue::Table(lua.create_table()?)) };
            let doc: LuaTable = view.get("doc")?;
            let line: i64 = {
                let sel: LuaMultiValue = doc.get::<LuaFunction>("get_selection")?.call(doc.clone())?;
                match sel.into_iter().next() { Some(LuaValue::Integer(n)) => n, _ => 1 }
            };
            let m: LuaTable = lua.registry_value(&mk)?;
            let sev_val: LuaValue = m.get::<LuaFunction>("get_line_diagnostic_severity")?.call((doc, line))?;
            if matches!(sev_val, LuaValue::Nil) {
                return Ok(LuaValue::Table(lua.create_table()?));
            }
            let style: LuaTable = req(lua, "core.style")?;
            let accent: LuaValue = style.get("accent")?;
            let text: LuaValue = style.get("text")?;
            let icon_font: LuaValue = style.get("icon_font")?;
            let t = lua.create_table()?;
            t.set(1, accent)?;
            t.set(2, icon_font)?;
            t.set(3, "!")?;
            t.set(4, text)?;
            t.set(5, " Quick Fix")?;
            Ok(LuaValue::Table(t))
        })?;

        let item = lua.create_table()?;
        item.set("predicate", pred)?;
        item.set("name", "lsp:quick-fix")?;
        let alignment: LuaValue = sv_item_cls.get("RIGHT")?;
        item.set("alignment", alignment)?;
        item.set("get_item", get_item)?;
        item.set("command", "lsp:quick-fix")?;
        item.set("tooltip", "Show quick fixes for the current diagnostic line")?;
        add_item.call::<()>((sv, item))?;
    }

    // Keymap bindings
    {
        let keymap: LuaTable = req(lua, "core.keymap")?;
        let bindings = lua.create_table()?;
        bindings.set("ctrl+space", "lsp:complete")?;
        bindings.set("f12", "lsp:goto-definition")?;
        bindings.set("ctrl+alt+left", "lsp:jump-back")?;
        bindings.set("ctrl+f12", "lsp:goto-type-definition")?;
        bindings.set("shift+f12", "lsp:find-references")?;
        bindings.set("f8", "lsp:next-diagnostic")?;
        bindings.set("shift+f8", "lsp:previous-diagnostic")?;
        bindings.set("ctrl+t", "lsp:show-document-symbols")?;
        bindings.set("ctrl+alt+t", "lsp:workspace-symbols")?;
        bindings.set("ctrl+shift+a", "lsp:code-action")?;
        bindings.set("alt+return", "lsp:quick-fix")?;
        bindings.set("ctrl+shift+space", "lsp:signature-help")?;
        bindings.set("alt+shift+f", "lsp:format-document")?;
        bindings.set("f2", "lsp:rename-symbol")?;
        bindings.set("ctrl+k", "lsp:hover")?;
        keymap.get::<LuaFunction>("add")?.call::<()>(bindings)?;
    }

    Ok(LuaValue::Table(manager))
}

/// Registers `plugins.lsp.json` as a native Rust module — replaces `plugins_lsp_json.lua`.
fn register_json(lua: &Lua, preload: &LuaTable) -> LuaResult<()> {
    preload.set(
        "plugins.lsp.json",
        lua.create_function(|lua, ()| -> LuaResult<LuaValue> {
            let native: LuaTable = lua.globals().get("lsp_protocol")?;
            let encode: LuaFunction = native.get("json_encode")?;
            let decode: LuaFunction = native.get("json_decode")?;
            let encode_safe_fn = encode.clone();
            let decode_safe_fn = decode.clone();
            let t = lua.create_table()?;
            t.set("encode", encode)?;
            t.set("decode", decode)?;
            t.set(
                "encode_safe",
                lua.create_function(move |lua, val: LuaValue| {
                    match encode_safe_fn.call::<LuaValue>(val) {
                        Ok(v) => Ok((true, v)),
                        Err(e) => Ok((false, LuaValue::String(lua.create_string(e.to_string())?))),
                    }
                })?,
            )?;
            t.set(
                "decode_safe",
                lua.create_function(move |lua, text: String| {
                    match decode_safe_fn.call::<LuaValue>(text) {
                        Ok(v) => Ok((true, v)),
                        Err(e) => Ok((false, LuaValue::String(lua.create_string(e.to_string())?))),
                    }
                })?,
            )?;
            Ok(LuaValue::Table(t))
        })?,
    )
}

/// Registers `plugins.lsp.protocol` as a native Rust module — replaces `plugins_lsp_protocol.lua`.
fn register_protocol(lua: &Lua, preload: &LuaTable) -> LuaResult<()> {
    preload.set(
        "plugins.lsp.protocol",
        lua.create_function(|lua, ()| -> LuaResult<LuaValue> {
            let native: LuaTable = lua.globals().get("lsp_protocol")?;
            let t = lua.create_table()?;
            t.set("completion_kinds", native.get::<LuaValue>("completion_kinds")?)?;
            t.set("encode_message", native.get::<LuaFunction>("encode_message")?)?;
            t.set("decode_messages", native.get::<LuaFunction>("decode_messages")?)?;
            Ok(LuaValue::Table(t))
        })?,
    )
}

/// Registers all LSP plugin modules as Rust-owned preloads.
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let package: LuaTable = lua.globals().get("package")?;
    let preload: LuaTable = package.get("preload")?;
    preload.set("plugins.lsp", lua.create_function(|lua, ()| init_lsp_plugin(lua))?)?;
    preload.set("plugins.lsp.client", lua.create_function(|lua, ()| init_client_module(lua))?)?;
    register_json(lua, &preload)?;
    register_protocol(lua, &preload)?;
    preload.set("plugins.lsp.server-manager", lua.create_function(|lua, ()| init_manager_module(lua))?)?;
    Ok(())
}
