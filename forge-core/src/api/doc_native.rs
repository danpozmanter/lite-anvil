use mlua::prelude::*;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use pcre2::bytes::Regex;
use std::collections::HashMap;
use std::fs;

#[derive(Clone)]
struct EditRecord {
    kind: u8,
    line1: usize,
    col1: usize,
    line2: usize,
    col2: usize,
    text: String,
}

struct BufferState {
    lines: Vec<String>,
    selections: Vec<usize>,
    undo: Vec<Vec<u8>>,
    redo: Vec<Vec<u8>>,
    change_id: i64,
    crlf: bool,
}

static BUFFERS: Lazy<Mutex<HashMap<u64, BufferState>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static NEXT_BUFFER_ID: Lazy<Mutex<u64>> = Lazy::new(|| Mutex::new(1));

fn next_buffer_id() -> u64 {
    let mut next = NEXT_BUFFER_ID.lock();
    let id = *next;
    *next += 1;
    id
}

fn default_buffer_state() -> BufferState {
    BufferState {
        lines: vec!["\n".to_string()],
        selections: vec![1, 1, 1, 1],
        undo: Vec::new(),
        redo: Vec::new(),
        change_id: 1,
        crlf: false,
    }
}

fn get_lines(lines: LuaTable) -> LuaResult<Vec<String>> {
    let mut out = Vec::new();
    for line in lines.sequence_values::<String>() {
        out.push(line?);
    }
    Ok(out)
}

fn set_lines(lua: &Lua, lines: &[String]) -> LuaResult<LuaTable> {
    let out = lua.create_table_with_capacity(lines.len(), 0)?;
    for (idx, line) in lines.iter().enumerate() {
        out.raw_set((idx + 1) as i64, line.as_str())?;
    }
    Ok(out)
}

fn get_selections(selections: LuaTable) -> LuaResult<Vec<usize>> {
    let mut out = Vec::new();
    for value in selections.sequence_values::<usize>() {
        out.push(value?);
    }
    Ok(out)
}

fn set_selections(lua: &Lua, selections: &[usize]) -> LuaResult<LuaTable> {
    let out = lua.create_table_with_capacity(selections.len(), 0)?;
    for (idx, value) in selections.iter().enumerate() {
        out.raw_set((idx + 1) as i64, *value)?;
    }
    Ok(out)
}

fn buffer_snapshot(lua: &Lua, state: &BufferState) -> LuaResult<LuaTable> {
    let out = lua.create_table()?;
    out.set("lines", set_lines(lua, &state.lines)?)?;
    out.set("selections", set_selections(lua, &state.selections)?)?;
    out.set("change_id", state.change_id)?;
    out.set("crlf", state.crlf)?;
    Ok(out)
}

fn with_buffer_mut<T, F>(buffer_id: u64, f: F) -> LuaResult<T>
where
    F: FnOnce(&mut BufferState) -> LuaResult<T>,
{
    let mut buffers = BUFFERS.lock();
    let state = buffers
        .get_mut(&buffer_id)
        .ok_or_else(|| LuaError::RuntimeError("unknown native doc buffer".to_string()))?;
    f(state)
}

fn split_lines(text: &str) -> Vec<String> {
    let mut res = Vec::new();
    for line in format!("{text}\n").split_terminator('\n') {
        res.push(line.to_string());
    }
    if res.is_empty() {
        res.push("\n".to_string());
    }
    res
}

fn sort_positions(
    line1: usize,
    col1: usize,
    line2: usize,
    col2: usize,
) -> (usize, usize, usize, usize) {
    if line1 > line2 || (line1 == line2 && col1 > col2) {
        (line2, col2, line1, col1)
    } else {
        (line1, col1, line2, col2)
    }
}

fn sanitize_position(lines: &[String], line: usize, col: usize) -> (usize, usize) {
    if lines.is_empty() {
        return (1, 1);
    }
    if line < 1 {
        return (1, 1);
    }
    if line > lines.len() {
        let last = lines.len();
        return (last, lines[last - 1].len().max(1));
    }
    (line, col.clamp(1, lines[line - 1].len().max(1)))
}

fn position_offset(
    lines: &[String],
    mut line: usize,
    mut col: usize,
    offset: isize,
) -> (usize, usize) {
    let mut remaining = offset;
    if lines.is_empty() {
        return (1, 1);
    }
    (line, col) = sanitize_position(lines, line, col);
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

fn get_text(
    lines: &[String],
    line1: usize,
    col1: usize,
    line2: usize,
    col2: usize,
    inclusive: bool,
) -> String {
    let (line1, col1) = sanitize_position(lines, line1, col1);
    let (line2, col2) = sanitize_position(lines, line2, col2);
    let (line1, col1, line2, col2) = sort_positions(line1, col1, line2, col2);
    let col2_offset = if inclusive { 0 } else { 1 };
    if line1 == line2 {
        return lines[line1 - 1]
            .get(col1 - 1..col2.saturating_sub(col2_offset))
            .unwrap_or("")
            .to_string();
    }

    let mut out = String::new();
    out.push_str(&lines[line1 - 1][col1 - 1..]);
    for line in lines.iter().take(line2 - 1).skip(line1) {
        out.push_str(line);
    }
    out.push_str(&lines[line2 - 1][..col2.saturating_sub(col2_offset)]);
    out
}

fn regex_find(
    line: &str,
    pattern: &str,
    no_case: bool,
    start_col: usize,
) -> Option<(usize, usize)> {
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

fn apply_insert_internal(
    lines: &mut Vec<String>,
    selections: &mut [usize],
    line: usize,
    col: usize,
    text: &str,
) {
    let mut insert_lines = split_lines(text);
    let len = insert_lines.last().map(|s| s.len()).unwrap_or(0);
    let before = lines[line - 1][..col - 1].to_string();
    let after = lines[line - 1][col - 1..].to_string();
    let split_count = insert_lines.len().saturating_sub(1);
    for item in insert_lines.iter_mut().take(split_count) {
        if !item.ends_with('\n') {
            item.push('\n');
        }
    }
    insert_lines[0] = format!("{before}{}", insert_lines[0]);
    let last_idx = insert_lines.len() - 1;
    insert_lines[last_idx].push_str(&after);

    lines.splice(line - 1..line, insert_lines.clone());

    for idx in (0..selections.len()).step_by(4).rev() {
        let cline1 = selections[idx];
        let ccol1 = selections[idx + 1];
        let cline2 = selections[idx + 2];
        let ccol2 = selections[idx + 3];
        if cline1 < line {
            break;
        }
        let line_addition = if line < cline1 || (line == cline1 && col < ccol1) {
            insert_lines.len() - 1
        } else {
            0
        };
        let column_addition = if line == cline1 && ccol1 > col {
            len
        } else {
            0
        };
        selections[idx] = cline1 + line_addition;
        selections[idx + 1] = ccol1 + column_addition;
        selections[idx + 2] = cline2 + line_addition;
        selections[idx + 3] = ccol2 + column_addition;
    }
}

fn apply_remove_internal(
    lines: &mut Vec<String>,
    selections: &mut Vec<usize>,
    line1: usize,
    col1: usize,
    line2: usize,
    col2: usize,
) {
    let before = lines[line1 - 1][..col1 - 1].to_string();
    let after = lines[line2 - 1][col2 - 1..].to_string();
    let line_removal = line2 - line1;
    let col_removal = col2 - col1;
    lines.splice(line1 - 1..line2, [format!("{before}{after}")]);

    let mut merge = false;
    let mut idx = selections.len();
    while idx >= 4 {
        idx -= 4;
        let cline1 = selections[idx];
        let ccol1 = selections[idx + 1];
        let cline2 = selections[idx + 2];
        let ccol2 = selections[idx + 3];
        if cline2 < line1 {
            break;
        }
        let (mut l1, mut c1, mut l2, mut c2) = (cline1, ccol1, cline2, ccol2);

        if cline1 > line1 || (cline1 == line1 && ccol1 > col1) {
            if cline1 > line2 {
                l1 -= line_removal;
            } else {
                l1 = line1;
                c1 = if cline1 == line2 && ccol1 > col2 {
                    c1 - col_removal
                } else {
                    col1
                };
            }
        }

        if cline2 > line1 || (cline2 == line1 && ccol2 > col1) {
            if cline2 > line2 {
                l2 -= line_removal;
            } else {
                l2 = line1;
                c2 = if cline2 == line2 && ccol2 > col2 {
                    c2 - col_removal
                } else {
                    col1
                };
            }
        }

        if l1 == line1 && c1 == col1 {
            merge = true;
        }
        selections[idx] = l1;
        selections[idx + 1] = c1;
        selections[idx + 2] = l2;
        selections[idx + 3] = c2;
    }

    if merge {
        merge_cursors(selections);
    }
}

fn merge_cursors(selections: &mut Vec<usize>) {
    let mut i = selections.len();
    while i >= 8 {
        i -= 4;
        let mut j = 0usize;
        while j + 4 <= i {
            if selections[i] == selections[j] && selections[i + 1] == selections[j + 1] {
                selections.drain(i..i + 4);
                break;
            }
            j += 4;
        }
    }
}

fn sanitize_selections(lines: &[String], selections: &mut [usize]) {
    for idx in (0..selections.len()).step_by(4) {
        let (l1, c1) = sanitize_position(lines, selections[idx], selections[idx + 1]);
        let (l2, c2) = sanitize_position(lines, selections[idx + 2], selections[idx + 3]);
        selections[idx] = l1;
        selections[idx + 1] = c1;
        selections[idx + 2] = l2;
        selections[idx + 3] = c2;
    }
}

fn put_u32(out: &mut Vec<u8>, value: usize) {
    out.extend_from_slice(&(value as u32).to_le_bytes());
}

fn read_u32(input: &[u8], offset: &mut usize) -> LuaResult<usize> {
    if *offset + 4 > input.len() {
        return Err(LuaError::RuntimeError("bad packed undo record".to_string()));
    }
    let value = u32::from_le_bytes(input[*offset..*offset + 4].try_into().unwrap()) as usize;
    *offset += 4;
    Ok(value)
}

fn pack_edit(out: &mut Vec<u8>, edit: &EditRecord) {
    out.push(edit.kind);
    put_u32(out, edit.line1);
    put_u32(out, edit.col1);
    put_u32(out, edit.line2);
    put_u32(out, edit.col2);
    put_u32(out, edit.text.len());
    out.extend_from_slice(edit.text.as_bytes());
}

fn unpack_edit(input: &[u8], offset: &mut usize) -> LuaResult<EditRecord> {
    if *offset >= input.len() {
        return Err(LuaError::RuntimeError("bad packed undo record".to_string()));
    }
    let kind = input[*offset];
    *offset += 1;
    let line1 = read_u32(input, offset)?;
    let col1 = read_u32(input, offset)?;
    let line2 = read_u32(input, offset)?;
    let col2 = read_u32(input, offset)?;
    let len = read_u32(input, offset)?;
    if *offset + len > input.len() {
        return Err(LuaError::RuntimeError("bad packed undo record".to_string()));
    }
    let text = String::from_utf8(input[*offset..*offset + len].to_vec())
        .map_err(|_| LuaError::RuntimeError("bad packed undo record".to_string()))?;
    *offset += len;
    Ok(EditRecord {
        kind,
        line1,
        col1,
        line2,
        col2,
        text,
    })
}

fn pack_record(selection_restore: &[usize], edits: &[EditRecord]) -> Vec<u8> {
    let mut out = Vec::new();
    put_u32(&mut out, selection_restore.len());
    for value in selection_restore {
        put_u32(&mut out, *value);
    }
    put_u32(&mut out, edits.len());
    for edit in edits {
        pack_edit(&mut out, edit);
    }
    out
}

fn unpack_record(input: &[u8]) -> LuaResult<(Vec<usize>, Vec<EditRecord>)> {
    let mut offset = 0usize;
    let count = read_u32(input, &mut offset)?;
    let mut selections = Vec::with_capacity(count);
    for _ in 0..count {
        selections.push(read_u32(input, &mut offset)?);
    }
    let edit_count = read_u32(input, &mut offset)?;
    let mut edits = Vec::with_capacity(edit_count);
    for _ in 0..edit_count {
        edits.push(unpack_edit(input, &mut offset)?);
    }
    Ok((selections, edits))
}

fn apply_single_edit(
    lines: &mut Vec<String>,
    selections: &mut Vec<usize>,
    edit: &EditRecord,
) -> EditRecord {
    match edit.kind {
        b'i' => {
            apply_insert_internal(lines, selections, edit.line1, edit.col1, &edit.text);
            sanitize_selections(lines, selections);
            EditRecord {
                kind: b'r',
                line1: edit.line1,
                col1: edit.col1,
                line2: position_offset(lines, edit.line1, edit.col1, edit.text.len() as isize).0,
                col2: position_offset(lines, edit.line1, edit.col1, edit.text.len() as isize).1,
                text: String::new(),
            }
        }
        _ => {
            let removed = get_text(lines, edit.line1, edit.col1, edit.line2, edit.col2, false);
            apply_remove_internal(
                lines, selections, edit.line1, edit.col1, edit.line2, edit.col2,
            );
            sanitize_selections(lines, selections);
            EditRecord {
                kind: b'i',
                line1: edit.line1,
                col1: edit.col1,
                line2: edit.line1,
                col2: edit.col1,
                text: removed,
            }
        }
    }
}

fn build_edit_result(
    lua: &Lua,
    lines: &[String],
    selections: &[usize],
    undo: Vec<u8>,
    line_delta: isize,
) -> LuaResult<LuaTable> {
    let out = lua.create_table()?;
    out.set("lines", set_lines(lua, lines)?)?;
    out.set("selections", set_selections(lua, selections)?)?;
    out.set("undo", lua.create_string(&undo)?)?;
    out.set("line_delta", line_delta)?;
    Ok(out)
}

fn make_insert_result(
    lua: &Lua,
    mut lines: Vec<String>,
    mut selections: Vec<usize>,
    line: usize,
    col: usize,
    text: String,
) -> LuaResult<LuaTable> {
    let selection_restore = selections.clone();
    let before_len = lines.len() as isize;
    apply_insert_internal(&mut lines, &mut selections, line, col, &text);
    sanitize_selections(&lines, &mut selections);
    let (line2, col2) = position_offset(&lines, line, col, text.len() as isize);
    let undo = pack_record(
        &selection_restore,
        &[EditRecord {
            kind: b'r',
            line1: line,
            col1: col,
            line2,
            col2,
            text: String::new(),
        }],
    );
    build_edit_result(
        lua,
        &lines,
        &selections,
        undo,
        lines.len() as isize - before_len,
    )
}

fn make_remove_result(
    lua: &Lua,
    mut lines: Vec<String>,
    mut selections: Vec<usize>,
    line1: usize,
    col1: usize,
    line2: usize,
    col2: usize,
) -> LuaResult<LuaTable> {
    let selection_restore = selections.clone();
    let before_len = lines.len() as isize;
    let removed = get_text(&lines, line1, col1, line2, col2, false);
    apply_remove_internal(&mut lines, &mut selections, line1, col1, line2, col2);
    sanitize_selections(&lines, &mut selections);
    let undo = pack_record(
        &selection_restore,
        &[EditRecord {
            kind: b'i',
            line1,
            col1,
            line2: line1,
            col2: col1,
            text: removed,
        }],
    );
    build_edit_result(
        lua,
        &lines,
        &selections,
        undo,
        lines.len() as isize - before_len,
    )
}

fn make_bulk_result(
    lua: &Lua,
    mut lines: Vec<String>,
    mut selections: Vec<usize>,
    edits: LuaTable,
) -> LuaResult<LuaTable> {
    let selection_restore = selections.clone();
    let before_len = lines.len() as isize;
    let mut inverse = Vec::new();
    for value in edits.sequence_values::<LuaTable>() {
        let edit = value?;
        let line1 = edit.get::<usize>("line1")?;
        let col1 = edit.get::<usize>("col1")?;
        let line2 = edit.get::<usize>("line2")?;
        let col2 = edit.get::<usize>("col2")?;
        let text = edit.get::<Option<String>>("text")?.unwrap_or_default();
        if line1 != line2 || col1 != col2 {
            let removed = get_text(&lines, line1, col1, line2, col2, false);
            apply_remove_internal(&mut lines, &mut selections, line1, col1, line2, col2);
            inverse.push(EditRecord {
                kind: b'i',
                line1,
                col1,
                line2: line1,
                col2: col1,
                text: removed,
            });
        }
        if !text.is_empty() {
            apply_insert_internal(&mut lines, &mut selections, line1, col1, &text);
            let (end_line, end_col) = position_offset(&lines, line1, col1, text.len() as isize);
            inverse.push(EditRecord {
                kind: b'r',
                line1,
                col1,
                line2: end_line,
                col2: end_col,
                text: String::new(),
            });
        }
    }
    inverse.reverse();
    sanitize_selections(&lines, &mut selections);
    let undo = pack_record(&selection_restore, &inverse);
    build_edit_result(
        lua,
        &lines,
        &selections,
        undo,
        lines.len() as isize - before_len,
    )
}

fn apply_packed_result(
    lua: &Lua,
    mut lines: Vec<String>,
    selections: Vec<usize>,
    packed: LuaString,
) -> LuaResult<LuaTable> {
    let (selection_restore, edits) = unpack_record(packed.as_bytes().as_ref())?;
    let before_len = lines.len() as isize;
    let mut working_selections = selections.clone();
    let mut inverse = Vec::new();
    for edit in &edits {
        inverse.push(apply_single_edit(&mut lines, &mut working_selections, edit));
    }
    inverse.reverse();
    let mut restored = selection_restore;
    sanitize_selections(&lines, &mut restored);
    let redo = pack_record(&selections, &inverse);
    build_edit_result(
        lua,
        &lines,
        &restored,
        redo,
        lines.len() as isize - before_len,
    )
}

fn clamp_history(history: &mut Vec<Vec<u8>>) {
    const MAX_UNDOS: usize = 10_000;
    if history.len() > MAX_UNDOS {
        let drop_count = history.len() - MAX_UNDOS;
        history.drain(0..drop_count);
    }
}

fn apply_record_to_state(state: &mut BufferState, packed: &[u8], push_redo: bool) -> LuaResult<()> {
    let (selection_restore, edits) = unpack_record(packed)?;
    let current_selection = state.selections.clone();
    let mut inverse = Vec::new();
    for edit in &edits {
        inverse.push(apply_single_edit(
            &mut state.lines,
            &mut state.selections,
            edit,
        ));
    }
    inverse.reverse();
    let mut restored = selection_restore;
    sanitize_selections(&state.lines, &mut restored);
    state.selections = restored;
    if push_redo {
        state.redo.push(pack_record(&current_selection, &inverse));
        clamp_history(&mut state.redo);
        state.change_id -= 1;
    } else {
        state.undo.push(pack_record(&current_selection, &inverse));
        clamp_history(&mut state.undo);
        state.change_id += 1;
    }
    Ok(())
}

fn apply_insert_to_buffer(state: &mut BufferState, line: usize, col: usize, text: &str) -> isize {
    let selection_restore = state.selections.clone();
    let before_len = state.lines.len() as isize;
    apply_insert_internal(&mut state.lines, &mut state.selections, line, col, text);
    sanitize_selections(&state.lines, &mut state.selections);
    let (line2, col2) = position_offset(&state.lines, line, col, text.len() as isize);
    state.undo.push(pack_record(
        &selection_restore,
        &[EditRecord {
            kind: b'r',
            line1: line,
            col1: col,
            line2,
            col2,
            text: String::new(),
        }],
    ));
    clamp_history(&mut state.undo);
    state.redo.clear();
    state.change_id += 1;
    state.lines.len() as isize - before_len
}

fn apply_remove_to_buffer(
    state: &mut BufferState,
    line1: usize,
    col1: usize,
    line2: usize,
    col2: usize,
) -> isize {
    let selection_restore = state.selections.clone();
    let before_len = state.lines.len() as isize;
    let removed = get_text(&state.lines, line1, col1, line2, col2, false);
    apply_remove_internal(
        &mut state.lines,
        &mut state.selections,
        line1,
        col1,
        line2,
        col2,
    );
    sanitize_selections(&state.lines, &mut state.selections);
    state.undo.push(pack_record(
        &selection_restore,
        &[EditRecord {
            kind: b'i',
            line1,
            col1,
            line2: line1,
            col2: col1,
            text: removed,
        }],
    ));
    clamp_history(&mut state.undo);
    state.redo.clear();
    state.change_id += 1;
    state.lines.len() as isize - before_len
}

fn apply_edits_to_buffer(state: &mut BufferState, edits: LuaTable) -> LuaResult<isize> {
    let selection_restore = state.selections.clone();
    let before_len = state.lines.len() as isize;
    let mut inverse = Vec::new();
    for value in edits.sequence_values::<LuaTable>() {
        let edit = value?;
        let line1 = edit.get::<usize>("line1")?;
        let col1 = edit.get::<usize>("col1")?;
        let line2 = edit.get::<usize>("line2")?;
        let col2 = edit.get::<usize>("col2")?;
        let text = edit.get::<Option<String>>("text")?.unwrap_or_default();
        if line1 != line2 || col1 != col2 {
            let removed = get_text(&state.lines, line1, col1, line2, col2, false);
            apply_remove_internal(
                &mut state.lines,
                &mut state.selections,
                line1,
                col1,
                line2,
                col2,
            );
            inverse.push(EditRecord {
                kind: b'i',
                line1,
                col1,
                line2: line1,
                col2: col1,
                text: removed,
            });
        }
        if !text.is_empty() {
            apply_insert_internal(&mut state.lines, &mut state.selections, line1, col1, &text);
            let (end_line, end_col) =
                position_offset(&state.lines, line1, col1, text.len() as isize);
            inverse.push(EditRecord {
                kind: b'r',
                line1,
                col1,
                line2: end_line,
                col2: end_col,
                text: String::new(),
            });
        }
    }
    inverse.reverse();
    sanitize_selections(&state.lines, &mut state.selections);
    state.undo.push(pack_record(&selection_restore, &inverse));
    clamp_history(&mut state.undo);
    state.redo.clear();
    state.change_id += 1;
    Ok(state.lines.len() as isize - before_len)
}

fn load_file_into_state(state: &mut BufferState, filename: &str) -> LuaResult<()> {
    let bytes = fs::read(filename).map_err(|e| LuaError::RuntimeError(e.to_string()))?;
    let content = String::from_utf8_lossy(&bytes).to_string();
    state.lines.clear();
    state.crlf = content.contains("\r\n");
    if content.is_empty() {
        state.lines.push("\n".to_string());
    } else {
        for line in content.split_inclusive('\n') {
            let line = if let Some(stripped) = line.strip_suffix("\r\n") {
                format!("{stripped}\n")
            } else {
                line.to_string()
            };
            state.lines.push(line);
        }
        if !content.ends_with('\n') {
            if let Some(last) = state.lines.last_mut() {
                last.push('\n');
            }
        }
        if state.lines.is_empty() {
            state.lines.push("\n".to_string());
        }
    }
    state.selections = vec![1, 1, 1, 1];
    state.undo.clear();
    state.redo.clear();
    state.change_id = 1;
    Ok(())
}

fn save_state_to_file(state: &BufferState, filename: &str, crlf: bool) -> LuaResult<()> {
    let mut text = String::new();
    for line in &state.lines {
        if crlf {
            text.push_str(&line.replace('\n', "\r\n"));
        } else {
            text.push_str(line);
        }
    }
    fs::write(filename, text).map_err(|e| LuaError::RuntimeError(e.to_string()))
}

pub fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    let module = lua.create_table()?;

    module.set(
        "position_offset",
        lua.create_function(
            |_, (lines, line, col, offset): (LuaTable, usize, usize, isize)| {
                let lines = get_lines(lines)?;
                Ok(position_offset(&lines, line, col, offset))
            },
        )?,
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
        lua.create_function(
            |lua, (text, old, new, opts): (String, String, String, Option<LuaTable>)| {
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
            },
        )?,
    )?;

    module.set(
        "apply_insert",
        lua.create_function(
            |lua, (lines, selections, line, col, text): (LuaTable, LuaTable, usize, usize, String)| {
                make_insert_result(lua, get_lines(lines)?, get_selections(selections)?, line, col, text)
            },
        )?,
    )?;

    module.set(
        "apply_remove",
        lua.create_function(
            |lua,
             (lines, selections, line1, col1, line2, col2): (
                LuaTable,
                LuaTable,
                usize,
                usize,
                usize,
                usize,
            )| {
                make_remove_result(
                    lua,
                    get_lines(lines)?,
                    get_selections(selections)?,
                    line1,
                    col1,
                    line2,
                    col2,
                )
            },
        )?,
    )?;

    module.set(
        "apply_edits",
        lua.create_function(
            |lua, (lines, selections, edits): (LuaTable, LuaTable, LuaTable)| {
                make_bulk_result(lua, get_lines(lines)?, get_selections(selections)?, edits)
            },
        )?,
    )?;

    module.set(
        "apply_packed_undo",
        lua.create_function(
            |lua, (lines, selections, packed): (LuaTable, LuaTable, LuaString)| {
                apply_packed_result(lua, get_lines(lines)?, get_selections(selections)?, packed)
            },
        )?,
    )?;

    module.set(
        "buffer_new",
        lua.create_function(|_, ()| {
            let id = next_buffer_id();
            BUFFERS.lock().insert(id, default_buffer_state());
            Ok(id)
        })?,
    )?;

    module.set(
        "buffer_reset",
        lua.create_function(|lua, buffer_id: u64| {
            with_buffer_mut(buffer_id, |state| {
                *state = default_buffer_state();
                buffer_snapshot(lua, state)
            })
        })?,
    )?;

    module.set(
        "buffer_free",
        lua.create_function(|_, buffer_id: u64| Ok(BUFFERS.lock().remove(&buffer_id).is_some()))?,
    )?;

    module.set(
        "buffer_snapshot",
        lua.create_function(|lua, buffer_id: u64| {
            with_buffer_mut(buffer_id, |state| buffer_snapshot(lua, state))
        })?,
    )?;

    module.set(
        "buffer_set_selections",
        lua.create_function(|_, (buffer_id, selections): (u64, LuaTable)| {
            with_buffer_mut(buffer_id, |state| {
                state.selections = get_selections(selections)?;
                sanitize_selections(&state.lines, &mut state.selections);
                Ok(true)
            })
        })?,
    )?;

    module.set(
        "buffer_position_offset",
        lua.create_function(
            |_, (buffer_id, line, col, offset): (u64, usize, usize, isize)| {
                with_buffer_mut(buffer_id, |state| {
                    Ok(position_offset(&state.lines, line, col, offset))
                })
            },
        )?,
    )?;

    module.set(
        "buffer_get_text",
        lua.create_function(
            |_,
             (buffer_id, line1, col1, line2, col2, inclusive): (
                u64,
                usize,
                usize,
                usize,
                usize,
                Option<bool>,
            )| {
                with_buffer_mut(buffer_id, |state| {
                    Ok(get_text(
                        &state.lines,
                        line1,
                        col1,
                        line2,
                        col2,
                        inclusive.unwrap_or(false),
                    ))
                })
            },
        )?,
    )?;

    module.set(
        "buffer_get_change_id",
        lua.create_function(|_, buffer_id: u64| {
            with_buffer_mut(buffer_id, |state| Ok(state.change_id))
        })?,
    )?;

    module.set(
        "buffer_load",
        lua.create_function(|lua, (buffer_id, filename): (u64, String)| {
            with_buffer_mut(buffer_id, |state| {
                load_file_into_state(state, &filename)?;
                buffer_snapshot(lua, state)
            })
        })?,
    )?;

    module.set(
        "buffer_save",
        lua.create_function(|_, (buffer_id, filename, crlf): (u64, String, bool)| {
            with_buffer_mut(buffer_id, |state| {
                save_state_to_file(state, &filename, crlf).map(|_| true)
            })
        })?,
    )?;

    module.set(
        "buffer_apply_insert",
        lua.create_function(
            |lua, (buffer_id, line, col, text): (u64, usize, usize, String)| {
                with_buffer_mut(buffer_id, |state| {
                    let line_delta = apply_insert_to_buffer(state, line, col, &text);
                    let out = buffer_snapshot(lua, state)?;
                    out.set("line_delta", line_delta)?;
                    Ok(out)
                })
            },
        )?,
    )?;

    module.set(
        "buffer_apply_remove",
        lua.create_function(
            |lua, (buffer_id, line1, col1, line2, col2): (u64, usize, usize, usize, usize)| {
                with_buffer_mut(buffer_id, |state| {
                    let line_delta = apply_remove_to_buffer(state, line1, col1, line2, col2);
                    let out = buffer_snapshot(lua, state)?;
                    out.set("line_delta", line_delta)?;
                    Ok(out)
                })
            },
        )?,
    )?;

    module.set(
        "buffer_apply_edits",
        lua.create_function(|lua, (buffer_id, edits): (u64, LuaTable)| {
            with_buffer_mut(buffer_id, |state| {
                let line_delta = apply_edits_to_buffer(state, edits)?;
                let out = buffer_snapshot(lua, state)?;
                out.set("line_delta", line_delta)?;
                Ok(out)
            })
        })?,
    )?;

    module.set(
        "buffer_undo",
        lua.create_function(|lua, buffer_id: u64| {
            with_buffer_mut(buffer_id, |state| {
                if let Some(record) = state.undo.pop() {
                    apply_record_to_state(state, &record, true)?;
                }
                buffer_snapshot(lua, state)
            })
        })?,
    )?;

    module.set(
        "buffer_redo",
        lua.create_function(|lua, buffer_id: u64| {
            with_buffer_mut(buffer_id, |state| {
                if let Some(record) = state.redo.pop() {
                    apply_record_to_state(state, &record, false)?;
                }
                buffer_snapshot(lua, state)
            })
        })?,
    )?;

    Ok(module)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_remove_adjust_selections() {
        let mut lines = vec!["abc\n".to_string()];
        let mut selections = vec![1, 3, 1, 3];
        apply_insert_internal(&mut lines, &mut selections, 1, 2, "ZZ");
        assert_eq!(lines, vec!["aZZbc\n".to_string()]);
        assert_eq!(selections, vec![1, 5, 1, 5]);

        apply_remove_internal(&mut lines, &mut selections, 1, 2, 1, 4);
        assert_eq!(lines, vec!["abc\n".to_string()]);
        assert_eq!(selections, vec![1, 3, 1, 3]);
    }

    #[test]
    fn packed_record_round_trips() {
        let original_lines = vec!["abc\n".to_string()];
        let original_selections = vec![1, 2, 1, 2];
        let undo = pack_record(
            &original_selections,
            &[EditRecord {
                kind: b'i',
                line1: 1,
                col1: 2,
                line2: 1,
                col2: 2,
                text: "ZZ".to_string(),
            }],
        );
        let (selection_restore, edits) = unpack_record(&undo).unwrap();
        assert_eq!(selection_restore, original_selections);
        assert_eq!(edits.len(), 1);

        let mut lines = original_lines.clone();
        let mut selections = original_selections.clone();
        let inverse = apply_single_edit(&mut lines, &mut selections, &edits[0]);
        assert_eq!(lines, vec!["aZZbc\n".to_string()]);
        assert_eq!(inverse.kind, b'r');

        let reverse = pack_record(&original_selections, &[inverse]);
        let (_, reverse_edits) = unpack_record(&reverse).unwrap();
        apply_single_edit(&mut lines, &mut selections, &reverse_edits[0]);
        assert_eq!(lines, original_lines);
    }

    #[test]
    fn buffer_undo_and_redo_round_trip() {
        let mut state = default_buffer_state();
        apply_insert_to_buffer(&mut state, 1, 1, "abc");
        assert_eq!(state.lines, vec!["abc\n".to_string()]);
        assert_eq!(state.change_id, 2);

        let record = state.undo.pop().unwrap();
        apply_record_to_state(&mut state, &record, true).unwrap();
        assert_eq!(state.lines, vec!["\n".to_string()]);
        assert_eq!(state.change_id, 1);

        let redo = state.redo.pop().unwrap();
        apply_record_to_state(&mut state, &redo, false).unwrap();
        assert_eq!(state.lines, vec!["abc\n".to_string()]);
        assert_eq!(state.change_id, 2);
    }

    #[test]
    fn buffer_load_and_save_round_trip() {
        let path = std::env::temp_dir().join("lite_anvil_doc_native_test.txt");
        std::fs::write(&path, "one\r\ntwo\r\n").unwrap();

        let mut state = default_buffer_state();
        load_file_into_state(&mut state, path.to_str().unwrap()).unwrap();
        assert_eq!(state.lines, vec!["one\n".to_string(), "two\n".to_string()]);
        assert!(state.crlf);

        save_state_to_file(&state, path.to_str().unwrap(), true).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert_eq!(saved, "one\r\ntwo\r\n");

        let _ = std::fs::remove_file(path);
    }
}
