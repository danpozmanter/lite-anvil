use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::time::Instant;

use once_cell::sync::Lazy;
use parking_lot::Mutex;

/// Byte Order Mark types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BomType {
    None,
    Utf8,
    Utf16Be,
    Utf16Le,
    Utf32Be,
    Utf32Le,
}

impl BomType {
    /// Raw BOM bytes for this type.
    pub fn as_bytes(&self) -> &'static [u8] {
        match self {
            BomType::None => b"",
            BomType::Utf8 => b"\xef\xbb\xbf",
            BomType::Utf16Be => b"\xfe\xff",
            BomType::Utf16Le => b"\xff\xfe",
            BomType::Utf32Be => b"\x00\x00\xfe\xff",
            BomType::Utf32Le => b"\xff\xfe\x00\x00",
        }
    }

    /// Detect a BOM at the start of `bytes`. Returns the type and byte length.
    pub fn from_bytes(bytes: &[u8]) -> (BomType, usize) {
        if bytes.starts_with(b"\xef\xbb\xbf") {
            (BomType::Utf8, 3)
        } else if bytes.starts_with(b"\x00\x00\xfe\xff") {
            (BomType::Utf32Be, 4)
        } else if bytes.starts_with(b"\xff\xfe\x00\x00") {
            (BomType::Utf32Le, 4)
        } else if bytes.starts_with(b"\xfe\xff") {
            (BomType::Utf16Be, 2)
        } else if bytes.starts_with(b"\xff\xfe") {
            (BomType::Utf16Le, 2)
        } else {
            (BomType::None, 0)
        }
    }

    /// String representation for serialization.
    pub fn as_str(&self) -> &'static str {
        match self {
            BomType::None => "none",
            BomType::Utf8 => "utf-8",
            BomType::Utf16Be => "utf-16-be",
            BomType::Utf16Le => "utf-16-le",
            BomType::Utf32Be => "utf-32-be",
            BomType::Utf32Le => "utf-32-le",
        }
    }

    /// Parse from string representation.
    pub fn from_str(s: &str) -> BomType {
        match s {
            "utf-8" => BomType::Utf8,
            "utf-16-be" => BomType::Utf16Be,
            "utf-16-le" => BomType::Utf16Le,
            "utf-32-be" => BomType::Utf32Be,
            "utf-32-le" => BomType::Utf32Le,
            _ => BomType::None,
        }
    }
}

static START_TIME: Lazy<Instant> = Lazy::new(Instant::now);

/// Monotonic time in seconds since first call.
pub fn now_secs() -> f64 {
    START_TIME.elapsed().as_secs_f64()
}

/// A single edit operation in an undo/redo record.
#[derive(Clone, Debug)]
pub struct EditRecord {
    pub kind: u8,
    pub line1: usize,
    pub col1: usize,
    pub line2: usize,
    pub col2: usize,
    pub text: String,
}

/// Max seconds between edits to merge them into one undo entry.
pub const UNDO_MERGE_TIMEOUT: f64 = 1.0;

/// Core document buffer state, independent of any Lua types.
pub struct BufferState {
    pub lines: Vec<String>,
    pub selections: Vec<usize>,
    pub undo: Vec<Vec<u8>>,
    pub redo: Vec<Vec<u8>>,
    pub change_id: i64,
    pub crlf: bool,
    pub bom: BomType,
    pub sig_cache: (i64, u32),
    pub last_edit: Option<(f64, usize, usize, bool, bool)>,
}

pub static BUFFERS: Lazy<Mutex<HashMap<u64, BufferState>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
static NEXT_BUFFER_ID: Lazy<Mutex<u64>> = Lazy::new(|| Mutex::new(1));

/// Allocate a new buffer ID.
pub fn next_buffer_id() -> u64 {
    let mut next = NEXT_BUFFER_ID.lock();
    let id = *next;
    *next += 1;
    id
}

/// Create a default empty buffer state.
pub fn default_buffer_state() -> BufferState {
    let lines = vec!["\n".to_string()];
    let sig = content_signature(&lines);
    BufferState {
        lines,
        selections: vec![1, 1, 1, 1],
        undo: Vec::new(),
        redo: Vec::new(),
        change_id: 1,
        crlf: false,
        bom: BomType::None,
        sig_cache: (1, sig),
        last_edit: None,
    }
}

/// Access a buffer by ID, calling `f` with a mutable reference.
pub fn with_buffer_mut<T>(
    buffer_id: u64,
    f: impl FnOnce(&mut BufferState) -> Result<T, BufferError>,
) -> Result<T, BufferError> {
    let mut buffers = BUFFERS.lock();
    let state = buffers
        .get_mut(&buffer_id)
        .ok_or(BufferError::UnknownBuffer)?;
    f(state)
}

/// Access a buffer by ID immutably.
pub fn with_buffer<T>(
    buffer_id: u64,
    f: impl FnOnce(&BufferState) -> Result<T, BufferError>,
) -> Result<T, BufferError> {
    let buffers = BUFFERS.lock();
    let state = buffers.get(&buffer_id).ok_or(BufferError::UnknownBuffer)?;
    f(state)
}

/// Insert a new buffer state, returning its ID.
pub fn insert_buffer(state: BufferState) -> u64 {
    let id = next_buffer_id();
    BUFFERS.lock().insert(id, state);
    id
}

/// Remove a buffer by ID.
pub fn remove_buffer(buffer_id: u64) {
    BUFFERS.lock().remove(&buffer_id);
}

/// Number of cursors in a buffer (each cursor occupies 4 entries in selections).
pub fn cursor_count(state: &BufferState) -> usize {
    state.selections.len() / 4
}

/// Append a new cursor at (line, col) with collapsed selection.
pub fn add_cursor(state: &mut BufferState, line: usize, col: usize) {
    state.selections.extend_from_slice(&[line, col, line, col]);
}

/// Keep only the first cursor, removing all extras.
pub fn remove_extra_cursors(state: &mut BufferState) {
    state.selections.truncate(4);
}

/// FNV-1a hash of all line contents for dirty detection.
pub fn content_signature(lines: &[String]) -> u32 {
    let mut hash: u32 = 2_166_136_261;
    for line in lines {
        for &b in line.as_bytes() {
            hash ^= b as u32;
            hash = hash.wrapping_mul(16_777_619);
        }
        hash ^= 10;
        hash = hash.wrapping_mul(16_777_619);
    }
    hash
}

/// Split text into lines, each ending with `\n`.
pub fn split_lines(text: &str) -> Vec<String> {
    let mut res = Vec::new();
    for line in format!("{text}\n").split_terminator('\n') {
        res.push(line.to_string());
    }
    if res.is_empty() {
        res.push("\n".to_string());
    }
    res
}

/// Sort two positions so (line1, col1) <= (line2, col2).
pub fn sort_positions(
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

/// Validate that selections contain one or more 4-value ranges.
pub fn validate_selection_shape(selections: &[usize]) -> Result<(), BufferError> {
    if selections.is_empty() || selections.len() % 4 != 0 {
        return Err(BufferError::InvalidSelections);
    }
    Ok(())
}

/// Clamp a column to a valid UTF-8 boundary within a line.
pub fn clamp_column_to_boundary(line: &str, col: usize) -> usize {
    let mut byte = col.clamp(1, line.len().max(1)).saturating_sub(1);
    while byte > 0 && !line.is_char_boundary(byte) {
        byte -= 1;
    }
    byte + 1
}

/// Sanitize a (line, col) position to be within bounds.
pub fn sanitize_position(lines: &[String], line: usize, col: usize) -> (usize, usize) {
    if lines.is_empty() {
        return (1, 1);
    }
    let line = line.clamp(1, lines.len());
    (line, clamp_column_to_boundary(&lines[line - 1], col))
}

/// Normalize and sort a range of positions.
pub fn normalize_range(
    lines: &[String],
    line1: usize,
    col1: usize,
    line2: usize,
    col2: usize,
) -> (usize, usize, usize, usize) {
    let (line1, col1) = sanitize_position(lines, line1, col1);
    let (line2, col2) = sanitize_position(lines, line2, col2);
    sort_positions(line1, col1, line2, col2)
}

/// Move a position by a byte offset, wrapping across lines.
pub fn position_offset(
    lines: &[String],
    mut line: usize,
    mut col: usize,
    offset: isize,
) -> (usize, usize) {
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

/// Extract text from a range of lines.
pub fn get_text(
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

/// Apply an insert operation to lines and adjust selections.
pub fn apply_insert_internal(
    lines: &mut Vec<String>,
    selections: &mut [usize],
    line: usize,
    col: usize,
    text: &str,
) {
    let mut insert_lines = split_lines(text);
    let len = insert_lines.last().map(|s| s.len()).unwrap_or(0);
    if insert_lines.len() == 1 {
        // Single-line insert: modify in place, no temporary allocations.
        lines[line - 1].insert_str(col - 1, &insert_lines[0]);
    } else {
        let after = lines[line - 1][col - 1..].to_string();
        let split_count = insert_lines.len().saturating_sub(1);
        for item in insert_lines.iter_mut().take(split_count) {
            if !item.ends_with('\n') {
                item.push('\n');
            }
        }
        // Prepend the portion before the insert point to the first new line.
        insert_lines[0].insert_str(0, &lines[line - 1][..col - 1]);
        let last_idx = insert_lines.len() - 1;
        insert_lines[last_idx].push_str(&after);
        lines.splice(line - 1..line, insert_lines.clone());
    }

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

/// Apply a remove operation to lines and adjust selections.
pub fn apply_remove_internal(
    lines: &mut Vec<String>,
    selections: &mut Vec<usize>,
    line1: usize,
    col1: usize,
    line2: usize,
    col2: usize,
) {
    let adjust_col_after_join = |col: usize| {
        if col > col2 {
            col1 + (col - col2)
        } else {
            col1
        }
    };
    let line_removal = line2 - line1;
    if line1 == line2 {
        lines[line1 - 1].replace_range(col1 - 1..col2 - 1, "");
    } else {
        let after = lines[line2 - 1][col2 - 1..].to_string();
        lines[line1 - 1].truncate(col1 - 1);
        lines[line1 - 1].push_str(&after);
        lines.drain(line1..line2);
    }

    let mut merge = false;
    let mut idx = selections.len();
    while idx >= 4 {
        idx -= 4;
        let (cline1, ccol1, cline2, ccol2) = (
            selections[idx],
            selections[idx + 1],
            selections[idx + 2],
            selections[idx + 3],
        );
        if cline2 < line1 {
            break;
        }
        let (mut l1, mut c1, mut l2, mut c2) = (cline1, ccol1, cline2, ccol2);

        if cline1 > line1 || (cline1 == line1 && ccol1 > col1) {
            if cline1 > line2 {
                l1 -= line_removal;
            } else {
                l1 = line1;
                c1 = if cline1 == line2 {
                    adjust_col_after_join(c1)
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
                c2 = if cline2 == line2 {
                    adjust_col_after_join(c2)
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

/// Merge duplicate cursor positions in selections.
pub fn merge_cursors(selections: &mut Vec<usize>) {
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

/// Sanitize all selection positions to be within buffer bounds.
pub fn sanitize_selections(lines: &[String], selections: &mut [usize]) {
    for idx in (0..selections.len()).step_by(4) {
        let (l1, c1) = sanitize_position(lines, selections[idx], selections[idx + 1]);
        let (l2, c2) = sanitize_position(lines, selections[idx + 2], selections[idx + 3]);
        selections[idx] = l1;
        selections[idx + 1] = c1;
        selections[idx + 2] = l2;
        selections[idx + 3] = c2;
    }
}

// ── Undo record serialization ────────────────────────────────────────────────

fn put_u32(out: &mut Vec<u8>, value: usize) {
    out.extend_from_slice(&(value as u32).to_le_bytes());
}

fn read_u32(input: &[u8], offset: &mut usize) -> Result<usize, BufferError> {
    if *offset + 4 > input.len() {
        return Err(BufferError::BadUndoRecord);
    }
    let value = u32::from_le_bytes(input[*offset..*offset + 4].try_into().unwrap()) as usize;
    *offset += 4;
    Ok(value)
}

/// Serialize an edit record into bytes.
pub fn pack_edit(out: &mut Vec<u8>, edit: &EditRecord) {
    out.push(edit.kind);
    put_u32(out, edit.line1);
    put_u32(out, edit.col1);
    put_u32(out, edit.line2);
    put_u32(out, edit.col2);
    put_u32(out, edit.text.len());
    out.extend_from_slice(edit.text.as_bytes());
}

/// Deserialize an edit record from bytes.
pub fn unpack_edit(input: &[u8], offset: &mut usize) -> Result<EditRecord, BufferError> {
    if *offset >= input.len() {
        return Err(BufferError::BadUndoRecord);
    }
    let kind = input[*offset];
    *offset += 1;
    let line1 = read_u32(input, offset)?;
    let col1 = read_u32(input, offset)?;
    let line2 = read_u32(input, offset)?;
    let col2 = read_u32(input, offset)?;
    let len = read_u32(input, offset)?;
    if *offset + len > input.len() {
        return Err(BufferError::BadUndoRecord);
    }
    let text = String::from_utf8(input[*offset..*offset + len].to_vec())
        .map_err(|_| BufferError::BadUndoRecord)?;
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

/// Pack a full undo record (selections + edits) into bytes.
pub fn pack_record(selection_restore: &[usize], edits: &[EditRecord]) -> Vec<u8> {
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

/// Unpack a full undo record from bytes.
pub fn unpack_record(input: &[u8]) -> Result<(Vec<usize>, Vec<EditRecord>), BufferError> {
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

/// Apply a single edit and return the inverse edit.
pub fn apply_single_edit(
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

/// Clamp undo/redo history to max entries.
pub fn clamp_history(history: &mut Vec<Vec<u8>>) {
    const MAX_UNDOS: usize = 2_000;
    if history.len() > MAX_UNDOS {
        let drop_count = history.len() - MAX_UNDOS;
        history.drain(0..drop_count);
        history.shrink_to_fit();
    }
}

/// Serialize undo+redo history to a byte blob for persistence.
pub fn serialize_history(undo: &[Vec<u8>], redo: &[Vec<u8>]) -> Vec<u8> {
    const PERSISTENT_UNDO_CAP: usize = 5 * 1024 * 1024;
    let mut total_size = 8usize;
    let mut undo_entries: Vec<&[u8]> = Vec::new();
    let mut redo_entries: Vec<&[u8]> = Vec::new();

    for entry in undo.iter().rev() {
        let entry_size = 4 + entry.len();
        if total_size + entry_size > PERSISTENT_UNDO_CAP {
            break;
        }
        total_size += entry_size;
        undo_entries.push(entry);
    }
    undo_entries.reverse();

    for entry in redo.iter().rev() {
        let entry_size = 4 + entry.len();
        if total_size + entry_size > PERSISTENT_UNDO_CAP {
            break;
        }
        total_size += entry_size;
        redo_entries.push(entry);
    }
    redo_entries.reverse();

    let mut out = Vec::with_capacity(total_size);
    out.extend_from_slice(&(undo_entries.len() as u32).to_le_bytes());
    out.extend_from_slice(&(redo_entries.len() as u32).to_le_bytes());
    for entry in &undo_entries {
        out.extend_from_slice(&(entry.len() as u32).to_le_bytes());
        out.extend_from_slice(entry);
    }
    for entry in &redo_entries {
        out.extend_from_slice(&(entry.len() as u32).to_le_bytes());
        out.extend_from_slice(entry);
    }
    out
}

/// Undo + redo history pair.
pub type HistoryPair = (Vec<Vec<u8>>, Vec<Vec<u8>>);

/// Deserialize undo+redo history from a byte blob.
pub fn deserialize_history(data: &[u8]) -> Option<HistoryPair> {
    if data.len() < 8 {
        return None;
    }
    let undo_count = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let redo_count = u32::from_le_bytes([data[4], data[5], data[6], data[7]]) as usize;
    let mut undo = Vec::with_capacity(undo_count);
    let mut redo = Vec::with_capacity(redo_count);
    let mut pos = 8usize;

    for _ in 0..undo_count {
        if pos + 4 > data.len() {
            return None;
        }
        let len =
            u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        pos += 4;
        if pos + len > data.len() {
            return None;
        }
        undo.push(data[pos..pos + len].to_vec());
        pos += len;
    }

    for _ in 0..redo_count {
        if pos + 4 > data.len() {
            return None;
        }
        let len =
            u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        pos += 4;
        if pos + len > data.len() {
            return None;
        }
        redo.push(data[pos..pos + len].to_vec());
        pos += len;
    }

    Some((undo, redo))
}

/// Reset undo/redo history.
pub fn reset_history(state: &mut BufferState) {
    state.undo.clear();
    state.redo.clear();
    state.undo.shrink_to_fit();
    state.redo.shrink_to_fit();
}

/// Load a file into buffer state.
pub fn load_file(state: &mut BufferState, filename: &str) -> Result<(), std::io::Error> {
    let bytes = fs::read(filename)?;
    let (bom, bom_len) = BomType::from_bytes(&bytes);
    state.bom = bom;
    let bytes_without_bom = if bom_len > 0 {
        &bytes[bom_len..]
    } else {
        &bytes
    };

    let content = String::from_utf8_lossy(bytes_without_bom).to_string();
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
    state.lines.shrink_to_fit();
    state.selections.shrink_to_fit();
    reset_history(state);
    state.change_id = 1;
    state.sig_cache = (0, 0);
    Ok(())
}

/// Save buffer state to a file (atomic write via temp file).
pub fn save_file(state: &BufferState, filename: &str, crlf: bool) -> Result<(), std::io::Error> {
    let path = std::path::Path::new(filename);
    let tmp = path.with_extension("tmp");
    let mut f = fs::File::create(&tmp)?;

    if state.bom != BomType::None {
        f.write_all(state.bom.as_bytes())?;
    }

    for line in &state.lines {
        let trimmed = line.trim_end_matches([' ', '\t']);
        if crlf {
            f.write_all(trimmed.replace('\n', "\r\n").as_bytes())?;
        } else {
            f.write_all(trimmed.as_bytes())?;
        }
    }
    f.sync_all()?;
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Snapshot the current buffer state for undo.
pub fn push_undo(state: &mut BufferState) {
    let snapshot = serde_json::json!({
        "lines": state.lines,
        "selections": state.selections,
        "change_id": state.change_id,
    });
    state.undo.push(snapshot.to_string().into_bytes());
    state.redo.clear();
    state.change_id += 1;
    state.last_edit = None;
    if state.undo.len() > 2_000 {
        state.undo.remove(0);
    }
}

/// Push undo for a single-char insert, merging consecutive keystrokes.
///
/// Returns `true` if the edit was merged (no new snapshot pushed), meaning the
/// caller should proceed with the insert but skip `push_undo`. Returns `false`
/// if a new snapshot was pushed normally.
pub fn push_undo_mergeable(
    state: &mut BufferState,
    line: usize,
    col: usize,
    has_selection: bool,
) -> bool {
    let now = now_secs();
    // Merge when: no selection, last edit was also a single-char insert on the
    // same line, adjacent column, and within the merge timeout.
    if !has_selection {
        if let Some((prev_time, prev_line, prev_col, true, false)) = state.last_edit {
            if line == prev_line
                && col == prev_col + 1
                && (now - prev_time) < UNDO_MERGE_TIMEOUT
            {
                state.last_edit = Some((now, line, col, true, false));
                state.change_id += 1;
                return true;
            }
        }
    }
    push_undo(state);
    state.last_edit = Some((now, line, col, true, has_selection));
    false
}

/// Undo the last edit.
pub fn undo(state: &mut BufferState) {
    let Some(snapshot) = state.undo.pop() else { return };
    // Save current state to redo.
    let current = serde_json::json!({
        "lines": state.lines,
        "selections": state.selections,
        "change_id": state.change_id,
    });
    state.redo.push(current.to_string().into_bytes());
    // Restore.
    if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&snapshot) {
        if let Some(lines) = val["lines"].as_array() {
            state.lines = lines.iter().filter_map(|v| v.as_str().map(String::from)).collect();
        }
        if let Some(sels) = val["selections"].as_array() {
            state.selections = sels.iter().filter_map(|v| v.as_u64().map(|n| n as usize)).collect();
        }
        if let Some(cid) = val["change_id"].as_i64() {
            state.change_id = cid;
        }
    }
    state.last_edit = None;
}

/// Redo the last undone edit.
pub fn redo(state: &mut BufferState) {
    let Some(snapshot) = state.redo.pop() else { return };
    let current = serde_json::json!({
        "lines": state.lines,
        "selections": state.selections,
        "change_id": state.change_id,
    });
    state.undo.push(current.to_string().into_bytes());
    if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&snapshot) {
        if let Some(lines) = val["lines"].as_array() {
            state.lines = lines.iter().filter_map(|v| v.as_str().map(String::from)).collect();
        }
        if let Some(sels) = val["selections"].as_array() {
            state.selections = sels.iter().filter_map(|v| v.as_u64().map(|n| n as usize)).collect();
        }
        if let Some(cid) = val["change_id"].as_i64() {
            state.change_id = cid;
        }
    }
    state.last_edit = None;
}

/// Get selected text range from buffer.
pub fn get_selected_text(state: &BufferState) -> String {
    if state.selections.len() < 4 {
        return String::new();
    }
    let (l1, c1, l2, c2) = (state.selections[0], state.selections[1], state.selections[2], state.selections[3]);
    if l1 == l2 && c1 == c2 {
        return String::new();
    }
    // Normalize so (l1,c1) <= (l2,c2).
    let (l1, c1, l2, c2) = if l1 > l2 || (l1 == l2 && c1 > c2) {
        (l2, c2, l1, c1)
    } else {
        (l1, c1, l2, c2)
    };
    let mut result = String::new();
    for line_num in l1..=l2 {
        if line_num > state.lines.len() { break; }
        let line = &state.lines[line_num - 1];
        let text = line.trim_end_matches('\n');
        let start = if line_num == l1 { c1.saturating_sub(1) } else { 0 };
        let end = if line_num == l2 { c2.saturating_sub(1).min(text.chars().count()) } else { text.chars().count() };
        let slice: String = text.chars().skip(start).take(end - start).collect();
        result.push_str(&slice);
        if line_num < l2 {
            result.push('\n');
        }
    }
    result
}

/// Delete the selected text range and collapse selection.
pub fn delete_selection(state: &mut BufferState) {
    if state.selections.len() < 4 { return; }
    let (l1, c1, l2, c2) = (state.selections[0], state.selections[1], state.selections[2], state.selections[3]);
    if l1 == l2 && c1 == c2 { return; }
    let (l1, c1, l2, c2) = if l1 > l2 || (l1 == l2 && c1 > c2) {
        (l2, c2, l1, c1)
    } else {
        (l1, c1, l2, c2)
    };

    if l1 == l2 {
        let line = &mut state.lines[l1 - 1];
        let text = line.trim_end_matches('\n').to_string();
        let has_nl = line.ends_with('\n');
        let before: String = text.chars().take(c1 - 1).collect();
        let after: String = text.chars().skip(c2 - 1).collect();
        *line = format!("{before}{after}{}", if has_nl { "\n" } else { "" });
    } else {
        let first_line = &state.lines[l1 - 1];
        let before: String = first_line.trim_end_matches('\n').chars().take(c1 - 1).collect();
        let last_line = &state.lines[l2 - 1];
        let after: String = last_line.trim_end_matches('\n').chars().skip(c2 - 1).collect();
        let has_nl = last_line.ends_with('\n');
        state.lines[l1 - 1] = format!("{before}{after}{}", if has_nl { "\n" } else { "" });
        state.lines.drain(l1..l2);
    }
    state.selections = vec![l1, c1, l1, c1];
}

/// Regex find within a line. Returns 1-based (start, end) column positions.
pub fn regex_find_in_line(
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
    let re = pcre2::bytes::Regex::new(&pat).ok()?;
    let mut locs = re.capture_locations();
    re.captures_read_at(&mut locs, line.as_bytes(), start_col.saturating_sub(1))
        .ok()
        .flatten()?;
    let (s, e) = locs.get(0)?;
    Some((s + 1, e + 1))
}

/// Plain text replacement. Returns (result, replacement_count).
pub fn replace_plain(text: &str, old: &str, new: &str) -> (String, usize) {
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

/// Regex replacement. Returns (result, replacement_count).
pub fn replace_regex(text: &str, pattern: &str, new: &str) -> Result<(String, usize), String> {
    let re = pcre2::bytes::Regex::new(pattern).map_err(|e| e.to_string())?;
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

/// Buffer operation errors.
#[derive(Debug, thiserror::Error)]
pub enum BufferError {
    #[error("unknown native doc buffer")]
    UnknownBuffer,
    #[error("selections must contain one or more 4-value ranges")]
    InvalidSelections,
    #[error("bad packed undo record")]
    BadUndoRecord,
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_signature_empty_vs_nonempty() {
        let empty = content_signature(&["\n".to_string()]);
        let nonempty = content_signature(&["hello\n".to_string()]);
        assert_ne!(empty, nonempty);
    }

    #[test]
    fn content_signature_deterministic() {
        let lines = vec!["hello\n".to_string(), "world\n".to_string()];
        assert_eq!(content_signature(&lines), content_signature(&lines));
    }

    #[test]
    fn split_lines_basic() {
        let lines = split_lines("hello\nworld");
        assert_eq!(lines, vec!["hello", "world"]);
    }

    #[test]
    fn split_lines_empty() {
        let lines = split_lines("");
        assert_eq!(lines, vec![""]);
    }

    #[test]
    fn sort_positions_already_sorted() {
        assert_eq!(sort_positions(1, 1, 2, 1), (1, 1, 2, 1));
    }

    #[test]
    fn sort_positions_reversed() {
        assert_eq!(sort_positions(2, 3, 1, 5), (1, 5, 2, 3));
    }

    #[test]
    fn validate_selection_shape_valid() {
        assert!(validate_selection_shape(&[1, 1, 1, 1]).is_ok());
        assert!(validate_selection_shape(&[1, 1, 1, 1, 2, 2, 2, 2]).is_ok());
    }

    #[test]
    fn validate_selection_shape_invalid() {
        assert!(validate_selection_shape(&[]).is_err());
        assert!(validate_selection_shape(&[1, 1, 1]).is_err());
    }

    #[test]
    fn position_offset_forward() {
        let lines = vec!["hello\n".to_string(), "world\n".to_string()];
        assert_eq!(position_offset(&lines, 1, 1, 3), (1, 4));
    }

    #[test]
    fn position_offset_across_lines() {
        let lines = vec!["ab\n".to_string(), "cd\n".to_string()];
        assert_eq!(position_offset(&lines, 1, 3, 1), (2, 1));
    }

    #[test]
    fn position_offset_backward() {
        let lines = vec!["hello\n".to_string()];
        assert_eq!(position_offset(&lines, 1, 4, -2), (1, 2));
    }

    #[test]
    fn get_text_single_line() {
        let lines = vec!["hello world\n".to_string()];
        assert_eq!(get_text(&lines, 1, 1, 1, 6, false), "hello");
    }

    #[test]
    fn get_text_multi_line() {
        let lines = vec!["hello\n".to_string(), "world\n".to_string()];
        let text = get_text(&lines, 1, 1, 2, 6, false);
        assert_eq!(text, "hello\nworld");
    }

    #[test]
    fn apply_insert_single_line() {
        let mut lines = vec!["hello\n".to_string()];
        let mut sel = vec![1, 1, 1, 1];
        apply_insert_internal(&mut lines, &mut sel, 1, 6, " world");
        assert_eq!(lines, vec!["hello world\n"]);
    }

    #[test]
    fn apply_insert_newline() {
        let mut lines = vec!["hello world\n".to_string()];
        let mut sel = vec![1, 6, 1, 6];
        apply_insert_internal(&mut lines, &mut sel, 1, 6, "\n");
        assert_eq!(lines, vec!["hello\n".to_string(), " world\n".to_string()]);
    }

    #[test]
    fn apply_remove_within_line() {
        let mut lines = vec!["hello world\n".to_string()];
        let mut sel = vec![1, 1, 1, 1];
        apply_remove_internal(&mut lines, &mut sel, 1, 6, 1, 12);
        assert_eq!(lines, vec!["hello\n"]);
    }

    #[test]
    fn apply_remove_across_lines() {
        let mut lines = vec!["hello\n".to_string(), "world\n".to_string()];
        let mut sel = vec![1, 1, 1, 1];
        apply_remove_internal(&mut lines, &mut sel, 1, 4, 2, 3);
        assert_eq!(lines, vec!["helrld\n"]);
    }

    #[test]
    fn pack_unpack_round_trip() {
        let sels = vec![1, 1, 1, 1];
        let edits = vec![EditRecord {
            kind: b'i',
            line1: 1,
            col1: 1,
            line2: 1,
            col2: 1,
            text: "hello".to_string(),
        }];
        let packed = pack_record(&sels, &edits);
        let (unpacked_sels, unpacked_edits) = unpack_record(&packed).unwrap();
        assert_eq!(unpacked_sels, sels);
        assert_eq!(unpacked_edits.len(), 1);
        assert_eq!(unpacked_edits[0].text, "hello");
    }

    #[test]
    fn bom_detection() {
        let (bom, len) = BomType::from_bytes(b"\xef\xbb\xbfhello");
        assert_eq!(bom, BomType::Utf8);
        assert_eq!(len, 3);
    }

    #[test]
    fn bom_none() {
        let (bom, len) = BomType::from_bytes(b"hello");
        assert_eq!(bom, BomType::None);
        assert_eq!(len, 0);
    }

    #[test]
    fn bom_round_trip() {
        for bt in [
            BomType::None,
            BomType::Utf8,
            BomType::Utf16Be,
            BomType::Utf16Le,
        ] {
            assert_eq!(BomType::from_str(bt.as_str()), bt);
        }
    }

    #[test]
    fn replace_plain_basic() {
        let (result, count) = replace_plain("hello world hello", "hello", "hi");
        assert_eq!(result, "hi world hi");
        assert_eq!(count, 2);
    }

    #[test]
    fn replace_regex_basic() {
        let (result, count) = replace_regex("abc 123 def 456", r"\d+", "#").unwrap();
        assert_eq!(result, "abc # def #");
        assert_eq!(count, 2);
    }

    #[test]
    fn load_and_save_round_trip() {
        let tmp = std::env::temp_dir().join("liteanvil_test_buffer_rt.txt");
        fs::write(&tmp, "hello\nworld\n").unwrap();
        let mut state = default_buffer_state();
        load_file(&mut state, tmp.to_str().unwrap()).unwrap();
        assert_eq!(state.lines, vec!["hello\n", "world\n"]);
        let out = std::env::temp_dir().join("liteanvil_test_buffer_rt_out.txt");
        save_file(&state, out.to_str().unwrap(), false).unwrap();
        assert_eq!(fs::read_to_string(&out).unwrap(), "hello\nworld\n");
        let _ = fs::remove_file(&tmp);
        let _ = fs::remove_file(&out);
    }
}
