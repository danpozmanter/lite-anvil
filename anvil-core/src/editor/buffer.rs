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

/// Buffers larger than this byte threshold switch to "huge file mode": undo
/// snapshots are disabled (`push_undo` becomes a no-op that only bumps
/// `change_id`) and dirty-checking short-circuits to a change-id comparison
/// instead of re-hashing the whole buffer every frame. 50 MB is large enough
/// that small/medium source files keep full behavior, while 2 GB files stay
/// responsive.
pub const HUGE_FILE_THRESHOLD: u64 = 50 * 1024 * 1024;

/// Total-byte budget for the combined undo+redo stacks. When pushing a new
/// snapshot would exceed this, oldest entries are evicted from the front of
/// `undo` until the total drops back under the cap. Acts as a safety net for
/// files just under `HUGE_FILE_THRESHOLD` that still get edited heavily.
pub const UNDO_MEMORY_BUDGET: usize = 64 * 1024 * 1024;

/// Core document buffer state, independent of any Lua types.
pub struct BufferState {
    pub lines: Vec<String>,
    pub selections: Vec<usize>,
    pub undo: Vec<Vec<u8>>,
    pub redo: Vec<Vec<u8>>,
    pub change_id: i64,
    /// Sum of the byte lengths of every line. Computed on load and refreshed
    /// after undo/redo restores. Drives `is_huge()` which gates `push_undo`
    /// into no-op mode on multi-GB files. Not maintained incrementally on
    /// every edit — the 50 MB threshold has enough slack that a session's
    /// worth of edits can't move a buffer across it.
    pub total_bytes: u64,
    pub crlf: bool,
    pub bom: BomType,
    pub sig_cache: (i64, u32),
    pub last_edit: Option<(f64, usize, usize, bool, bool)>,
}

impl BufferState {
    /// True when this buffer has crossed the huge-file threshold. Huge
    /// buffers skip undo snapshots entirely and short-circuit dirty checks.
    pub fn is_huge(&self) -> bool {
        self.total_bytes > HUGE_FILE_THRESHOLD
    }
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
        total_bytes: 1,
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

/// Cached content signature keyed by `change_id`. Repeat calls for the same
/// `change_id` return the memoized hash without rescanning the buffer; edits
/// bump `change_id` which invalidates the cache. This is the only function
/// that should touch `state.sig_cache`.
pub fn content_signature_cached(state: &mut BufferState) -> u32 {
    if state.sig_cache.0 == state.change_id {
        return state.sig_cache.1;
    }
    let sig = content_signature(&state.lines);
    state.sig_cache = (state.change_id, sig);
    sig
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
    load_file_with_progress(state, filename, |_, _| {})
}

/// Load a file and call `progress(bytes_read, total_bytes)` periodically.
/// Uses a streaming read to avoid double-buffering large files in memory.
/// A 2 GB file loads with ~2 GB peak RSS (just the `lines` vec) instead of
/// ~6 GB with the old implementation which copied the file multiple times.
pub fn load_file_with_progress<F: FnMut(u64, u64)>(
    state: &mut BufferState,
    filename: &str,
    mut progress: F,
) -> Result<(), std::io::Error> {
    use std::io::{BufRead, BufReader};
    let file = fs::File::open(filename)?;
    let total = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut reader = BufReader::with_capacity(1 << 20, file);

    // Detect BOM from the first read.
    let mut peek = [0u8; 4];
    let peeked = {
        use std::io::Read;
        let mut f = fs::File::open(filename)?;
        f.read(&mut peek).unwrap_or(0)
    };
    let (bom, bom_len) = BomType::from_bytes(&peek[..peeked]);
    state.bom = bom;
    if bom_len > 0 {
        use std::io::Read;
        // Skip BOM bytes in the BufReader.
        let mut skip = vec![0u8; bom_len];
        reader.read_exact(&mut skip)?;
    }

    state.lines.clear();
    // Reserve capacity for typical file (avg line ~70 bytes). For a 2 GB file
    // that's ~30M lines -- reserving upfront avoids billions of small reallocs.
    if total > 0 {
        state.lines.reserve((total / 70).min(50_000_000) as usize);
    }

    let mut crlf_detected = false;
    let mut bytes_read: u64 = bom_len as u64;
    let mut progress_tick: u64 = 0;
    let mut line_bytes = Vec::with_capacity(256);
    loop {
        line_bytes.clear();
        let n = reader.read_until(b'\n', &mut line_bytes)?;
        if n == 0 {
            break;
        }
        bytes_read += n as u64;
        // Detect CRLF once and strip \r before the \n on every line.
        if !crlf_detected && line_bytes.len() >= 2 && line_bytes.ends_with(b"\r\n") {
            crlf_detected = true;
        }
        // Normalize line: strip \r from \r\n.
        let s = if line_bytes.len() >= 2 && line_bytes.ends_with(b"\r\n") {
            let without_cr = &line_bytes[..line_bytes.len() - 2];
            let mut s = String::from_utf8_lossy(without_cr).into_owned();
            s.push('\n');
            s
        } else {
            String::from_utf8_lossy(&line_bytes).into_owned()
        };
        state.lines.push(s);

        // Report progress every ~4 MB.
        progress_tick += n as u64;
        if progress_tick >= (4 << 20) {
            progress(bytes_read, total);
            progress_tick = 0;
        }
    }
    state.crlf = crlf_detected;
    // Ensure last line ends with '\n'.
    if let Some(last) = state.lines.last_mut() {
        if !last.ends_with('\n') {
            last.push('\n');
        }
    }
    if state.lines.is_empty() {
        state.lines.push("\n".to_string());
    }
    state.selections = vec![1, 1, 1, 1];
    state.lines.shrink_to_fit();
    state.selections.shrink_to_fit();
    reset_history(state);
    state.change_id = 1;
    state.total_bytes = state.lines.iter().map(|l| l.len() as u64).sum();
    state.sig_cache = (0, 0);
    progress(bytes_read, total);
    Ok(())
}

/// Save buffer state to a file.
///
/// `atomic = false` writes the new content over the existing inode with
/// truncate-then-write, which naturally preserves mode bits, ownership,
/// ACLs, xattrs, and hardlinks.
///
/// `atomic = true` writes to a sibling temp file and renames it over the
/// target, so a crash mid-write leaves the original intact. Because
/// rename installs a fresh inode, file metadata is explicitly copied
/// from the original before the rename (permissions on all platforms;
/// ownership and xattrs on Unix).
pub fn save_file(
    state: &BufferState,
    filename: &str,
    crlf: bool,
    atomic: bool,
) -> Result<(), std::io::Error> {
    if atomic {
        save_file_atomic(state, filename, crlf)
    } else {
        save_file_inplace(state, filename, crlf)
    }
}

fn write_content<W: Write>(
    mut w: W,
    state: &BufferState,
    crlf: bool,
) -> Result<(), std::io::Error> {
    if state.bom != BomType::None {
        w.write_all(state.bom.as_bytes())?;
    }
    for line in &state.lines {
        let trimmed = line.trim_end_matches([' ', '\t']);
        if crlf {
            w.write_all(trimmed.replace('\n', "\r\n").as_bytes())?;
        } else {
            w.write_all(trimmed.as_bytes())?;
        }
    }
    Ok(())
}

fn save_file_inplace(
    state: &BufferState,
    filename: &str,
    crlf: bool,
) -> Result<(), std::io::Error> {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(filename)?;
    write_content(&mut f, state, crlf)?;
    f.sync_all()?;
    Ok(())
}

fn save_file_atomic(
    state: &BufferState,
    filename: &str,
    crlf: bool,
) -> Result<(), std::io::Error> {
    let path = std::path::Path::new(filename);
    let orig_meta = fs::metadata(path).ok();

    let tmp = path.with_extension("tmp");
    let mut f = fs::File::create(&tmp)?;
    write_content(&mut f, state, crlf)?;
    f.sync_all()?;
    drop(f);

    if let Some(meta) = orig_meta {
        // Permissions (mode bits) — portable.
        let _ = fs::set_permissions(&tmp, meta.permissions());
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let _ = set_owner(&tmp, meta.uid(), meta.gid());
        }
        // Extended attributes — also covers POSIX ACLs (system.posix_acl_*)
        // and SELinux labels (security.selinux) on Linux.
        #[cfg(target_os = "linux")]
        {
            let _ = copy_xattrs(path, &tmp);
        }
    }

    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(unix)]
fn set_owner(path: &std::path::Path, uid: u32, gid: u32) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    // SAFETY: c_path is a valid NUL-terminated C string pointing to an
    // existing filesystem path; uid and gid are plain integers. chown
    // does not retain the pointer after returning.
    let rc = unsafe { libc::chown(c_path.as_ptr(), uid as libc::uid_t, gid as libc::gid_t) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn copy_xattrs(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let src_c = CString::new(src.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let dst_c = CString::new(dst.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;

    // Sizing call: passing (NULL, 0) returns the required buffer size.
    // SAFETY: src_c is a valid C string; listxattr writes nothing when
    // given a null/zero-length buffer.
    let sz = unsafe { libc::listxattr(src_c.as_ptr(), std::ptr::null_mut(), 0) };
    if sz < 0 {
        let err = std::io::Error::last_os_error();
        // ENOTSUP: filesystem has no xattr support — nothing to copy.
        if err.raw_os_error() == Some(libc::ENOTSUP) {
            return Ok(());
        }
        return Err(err);
    }
    if sz == 0 {
        return Ok(());
    }

    let mut names = vec![0u8; sz as usize];
    // SAFETY: names has capacity sz; listxattr writes at most `sz` bytes.
    let actual = unsafe {
        libc::listxattr(
            src_c.as_ptr(),
            names.as_mut_ptr() as *mut libc::c_char,
            names.len(),
        )
    };
    if actual < 0 {
        return Err(std::io::Error::last_os_error());
    }
    names.truncate(actual as usize);

    for raw_name in names.split(|&b| b == 0).filter(|s| !s.is_empty()) {
        let Ok(name_c) = CString::new(raw_name) else {
            continue;
        };
        // SAFETY: src_c and name_c are valid C strings; NULL buffer
        // requests the value size.
        let vsz = unsafe {
            libc::getxattr(src_c.as_ptr(), name_c.as_ptr(), std::ptr::null_mut(), 0)
        };
        if vsz < 0 {
            continue;
        }
        let mut value = vec![0u8; vsz as usize];
        // SAFETY: buffer has capacity vsz.
        let got = unsafe {
            libc::getxattr(
                src_c.as_ptr(),
                name_c.as_ptr(),
                value.as_mut_ptr() as *mut libc::c_void,
                value.len(),
            )
        };
        if got < 0 {
            continue;
        }
        value.truncate(got as usize);
        // SAFETY: dst_c and name_c are valid C strings; value pointer is
        // valid for `value.len()` bytes.
        unsafe {
            libc::setxattr(
                dst_c.as_ptr(),
                name_c.as_ptr(),
                value.as_ptr() as *const libc::c_void,
                value.len(),
                0,
            );
        }
    }

    Ok(())
}

/// Drop snapshots from the front of `undo` until the combined byte size of
/// `undo` + `redo` fits within `UNDO_MEMORY_BUDGET`. Guarantees that at least
/// one entry is retained even if a single snapshot exceeds the budget, so
/// the most recent edit is always undoable.
fn trim_history_to_budget(state: &mut BufferState) {
    let redo_bytes: usize = state.redo.iter().map(|e| e.len()).sum();
    let mut undo_bytes: usize = state.undo.iter().map(|e| e.len()).sum();
    while state.undo.len() > 1 && undo_bytes + redo_bytes > UNDO_MEMORY_BUDGET {
        let dropped = state.undo.remove(0).len();
        undo_bytes = undo_bytes.saturating_sub(dropped);
    }
}

/// Snapshot the current buffer state for undo.
///
/// On buffers over `HUGE_FILE_THRESHOLD` this becomes a no-op that only
/// advances `change_id` and clears the redo stack — at multi-GB file sizes,
/// JSON-serializing every line on each keystroke would lock up the editor.
/// Callers don't need to special-case huge files; the function handles it.
pub fn push_undo(state: &mut BufferState) {
    if state.is_huge() {
        state.redo.clear();
        state.change_id += 1;
        state.last_edit = None;
        return;
    }
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
    trim_history_to_budget(state);
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
            if line == prev_line && col == prev_col + 1 && (now - prev_time) < UNDO_MERGE_TIMEOUT {
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
    let Some(snapshot) = state.undo.pop() else {
        return;
    };
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
            state.lines = lines
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
        }
        if let Some(sels) = val["selections"].as_array() {
            state.selections = sels
                .iter()
                .filter_map(|v| v.as_u64().map(|n| n as usize))
                .collect();
        }
        if let Some(cid) = val["change_id"].as_i64() {
            state.change_id = cid;
        }
    }
    state.total_bytes = state.lines.iter().map(|l| l.len() as u64).sum();
    state.last_edit = None;
}

/// Redo the last undone edit.
pub fn redo(state: &mut BufferState) {
    let Some(snapshot) = state.redo.pop() else {
        return;
    };
    let current = serde_json::json!({
        "lines": state.lines,
        "selections": state.selections,
        "change_id": state.change_id,
    });
    state.undo.push(current.to_string().into_bytes());
    if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&snapshot) {
        if let Some(lines) = val["lines"].as_array() {
            state.lines = lines
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
        }
        if let Some(sels) = val["selections"].as_array() {
            state.selections = sels
                .iter()
                .filter_map(|v| v.as_u64().map(|n| n as usize))
                .collect();
        }
        if let Some(cid) = val["change_id"].as_i64() {
            state.change_id = cid;
        }
    }
    state.total_bytes = state.lines.iter().map(|l| l.len() as u64).sum();
    state.last_edit = None;
}

/// Get selected text range from buffer.
pub fn get_selected_text(state: &BufferState) -> String {
    if state.selections.len() < 4 {
        return String::new();
    }
    let (l1, c1, l2, c2) = (
        state.selections[0],
        state.selections[1],
        state.selections[2],
        state.selections[3],
    );
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
        if line_num > state.lines.len() {
            break;
        }
        let line = &state.lines[line_num - 1];
        let text = line.trim_end_matches('\n');
        let start = if line_num == l1 {
            c1.saturating_sub(1)
        } else {
            0
        };
        let end = if line_num == l2 {
            c2.saturating_sub(1).min(text.chars().count())
        } else {
            text.chars().count()
        };
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
    if state.selections.len() < 4 {
        return;
    }
    let (l1, c1, l2, c2) = (
        state.selections[0],
        state.selections[1],
        state.selections[2],
        state.selections[3],
    );
    if l1 == l2 && c1 == c2 {
        return;
    }
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
        let before: String = first_line
            .trim_end_matches('\n')
            .chars()
            .take(c1 - 1)
            .collect();
        let last_line = &state.lines[l2 - 1];
        let after: String = last_line
            .trim_end_matches('\n')
            .chars()
            .skip(c2 - 1)
            .collect();
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
        save_file(&state, out.to_str().unwrap(), false, false).unwrap();
        assert_eq!(fs::read_to_string(&out).unwrap(), "hello\nworld\n");
        let _ = fs::remove_file(&tmp);
        let _ = fs::remove_file(&out);
    }

    #[cfg(unix)]
    #[test]
    fn inplace_save_preserves_executable_bit() {
        use std::os::unix::fs::PermissionsExt;
        let path =
            std::env::temp_dir().join("liteanvil_test_inplace_exec.sh");
        fs::write(&path, "#!/bin/sh\necho old\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();

        let mut state = default_buffer_state();
        load_file(&mut state, path.to_str().unwrap()).unwrap();
        state.lines = vec!["#!/bin/sh\n".into(), "echo new\n".into()];
        save_file(&state, path.to_str().unwrap(), false, false).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
        let _ = fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_save_preserves_executable_bit() {
        use std::os::unix::fs::PermissionsExt;
        let path =
            std::env::temp_dir().join("liteanvil_test_atomic_exec.sh");
        fs::write(&path, "#!/bin/sh\necho old\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();

        let mut state = default_buffer_state();
        load_file(&mut state, path.to_str().unwrap()).unwrap();
        state.lines = vec!["#!/bin/sh\n".into(), "echo new\n".into()];
        save_file(&state, path.to_str().unwrap(), false, true).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
        let _ = fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn inplace_save_preserves_inode() {
        use std::os::unix::fs::MetadataExt;
        let path =
            std::env::temp_dir().join("liteanvil_test_inplace_inode.txt");
        fs::write(&path, "hello\n").unwrap();
        let ino_before = fs::metadata(&path).unwrap().ino();

        let mut state = default_buffer_state();
        load_file(&mut state, path.to_str().unwrap()).unwrap();
        state.lines = vec!["world\n".into()];
        save_file(&state, path.to_str().unwrap(), false, false).unwrap();

        let ino_after = fs::metadata(&path).unwrap().ino();
        assert_eq!(ino_before, ino_after, "in-place save must not replace inode");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn undo_restores_previous_state() {
        let mut state = default_buffer_state();
        state.lines = vec!["hello\n".to_string()];
        push_undo(&mut state);
        state.lines = vec!["hello world\n".to_string()];

        undo(&mut state);
        assert_eq!(state.lines, vec!["hello\n"]);
    }

    #[test]
    fn redo_restores_undone_state() {
        let mut state = default_buffer_state();
        state.lines = vec!["v1\n".to_string()];
        push_undo(&mut state);
        state.lines = vec!["v2\n".to_string()];

        undo(&mut state);
        assert_eq!(state.lines, vec!["v1\n"]);
        redo(&mut state);
        assert_eq!(state.lines, vec!["v2\n"]);
    }

    #[test]
    fn undo_redo_full_round_trip() {
        let mut state = default_buffer_state();
        state.lines = vec!["a\n".to_string()];

        push_undo(&mut state);
        state.lines = vec!["ab\n".to_string()];
        push_undo(&mut state);
        state.lines = vec!["abc\n".to_string()];
        push_undo(&mut state);
        state.lines = vec!["abcd\n".to_string()];

        // Undo all the way back.
        undo(&mut state);
        assert_eq!(state.lines, vec!["abc\n"]);
        undo(&mut state);
        assert_eq!(state.lines, vec!["ab\n"]);
        undo(&mut state);
        assert_eq!(state.lines, vec!["a\n"]);

        // Redo all the way forward.
        redo(&mut state);
        assert_eq!(state.lines, vec!["ab\n"]);
        redo(&mut state);
        assert_eq!(state.lines, vec!["abc\n"]);
        redo(&mut state);
        assert_eq!(state.lines, vec!["abcd\n"]);
    }

    #[test]
    fn undo_at_empty_history_is_noop() {
        let mut state = default_buffer_state();
        state.lines = vec!["hello\n".to_string()];
        let before = state.lines.clone();
        undo(&mut state);
        assert_eq!(state.lines, before);
    }

    #[test]
    fn redo_at_empty_history_is_noop() {
        let mut state = default_buffer_state();
        state.lines = vec!["hello\n".to_string()];
        let before = state.lines.clone();
        redo(&mut state);
        assert_eq!(state.lines, before);
    }

    #[test]
    fn push_undo_clears_redo_stack() {
        let mut state = default_buffer_state();
        state.lines = vec!["v1\n".to_string()];
        push_undo(&mut state);
        state.lines = vec!["v2\n".to_string()];
        undo(&mut state);
        assert_eq!(state.redo.len(), 1);

        // Editing after an undo should drop the redo stack ("forking" history).
        push_undo(&mut state);
        state.lines = vec!["v1-fork\n".to_string()];
        assert!(state.redo.is_empty());
    }

    #[test]
    fn reset_history_clears_both_stacks() {
        let mut state = default_buffer_state();
        push_undo(&mut state);
        state.lines = vec!["edited\n".to_string()];
        push_undo(&mut state);
        undo(&mut state);

        assert!(!state.undo.is_empty());
        assert!(!state.redo.is_empty());
        reset_history(&mut state);
        assert!(state.undo.is_empty());
        assert!(state.redo.is_empty());
    }

    #[test]
    fn clamp_history_under_cap_keeps_all() {
        let mut history: Vec<Vec<u8>> = (0..100).map(|i| vec![i as u8]).collect();
        clamp_history(&mut history);
        assert_eq!(history.len(), 100);
    }

    #[test]
    fn clamp_history_at_cap_keeps_all() {
        let mut history: Vec<Vec<u8>> = (0..2_000).map(|i| vec![(i % 256) as u8]).collect();
        clamp_history(&mut history);
        assert_eq!(history.len(), 2_000);
    }

    #[test]
    fn clamp_history_over_cap_drops_oldest() {
        let mut history: Vec<Vec<u8>> = (0..2_005).map(|i| vec![(i % 256) as u8]).collect();
        clamp_history(&mut history);
        assert_eq!(history.len(), 2_000);
        // The oldest 5 entries (0..5) should have been dropped; entry 0 is now what was index 5.
        assert_eq!(history[0], vec![5u8]);
    }

    #[test]
    fn serialize_deserialize_history_round_trip() {
        let undo: Vec<Vec<u8>> = vec![
            b"snapshot-one".to_vec(),
            b"snapshot-two-with-more-bytes".to_vec(),
            vec![0u8, 1, 2, 3, 255],
        ];
        let redo: Vec<Vec<u8>> = vec![b"redo-a".to_vec(), b"".to_vec()];
        let blob = serialize_history(&undo, &redo);
        let (out_undo, out_redo) = deserialize_history(&blob).expect("deserialize failed");
        assert_eq!(out_undo, undo);
        assert_eq!(out_redo, redo);
    }

    #[test]
    fn serialize_deserialize_empty_history() {
        let blob = serialize_history(&[], &[]);
        let (u, r) = deserialize_history(&blob).expect("deserialize failed");
        assert!(u.is_empty());
        assert!(r.is_empty());
    }

    #[test]
    fn deserialize_history_rejects_short_input() {
        assert!(deserialize_history(&[]).is_none());
        assert!(deserialize_history(&[0, 0, 0]).is_none()); // less than 8-byte header
    }

    #[test]
    fn deserialize_history_rejects_truncated_entry() {
        // Header claims 1 undo, 0 redo; entry length claims 100 but no payload.
        let mut bad = Vec::new();
        bad.extend_from_slice(&1u32.to_le_bytes());
        bad.extend_from_slice(&0u32.to_le_bytes());
        bad.extend_from_slice(&100u32.to_le_bytes());
        // No payload follows.
        assert!(deserialize_history(&bad).is_none());
    }

    #[test]
    fn serialize_history_caps_at_5mb() {
        // One entry that itself exceeds the 5MB cap should be omitted entirely.
        let huge = vec![0u8; 6 * 1024 * 1024];
        let blob = serialize_history(&[huge], &[]);
        let (u, r) = deserialize_history(&blob).expect("deserialize failed");
        // Cap kicks in: huge entry skipped.
        assert!(u.is_empty());
        assert!(r.is_empty());
    }

    #[test]
    fn serialize_history_drops_oldest_undo_first_under_cap() {
        // 3 small recent entries + 1 huge old entry: the huge one should be dropped, the small kept.
        let huge = vec![0u8; 6 * 1024 * 1024];
        let small_a = b"recent-a".to_vec();
        let small_b = b"recent-b".to_vec();
        let small_c = b"recent-c".to_vec();
        let undo = vec![huge, small_a.clone(), small_b.clone(), small_c.clone()];
        let blob = serialize_history(&undo, &[]);
        let (u, _) = deserialize_history(&blob).expect("deserialize failed");
        // The serializer iterates from most recent backward, so the 3 small entries fit; the huge one breaks the loop.
        assert_eq!(u, vec![small_a, small_b, small_c]);
    }

    #[test]
    fn push_undo_on_real_state_is_recoverable() {
        // End-to-end: drive push_undo via the real BufferState helper, then round-trip.
        let mut state = default_buffer_state();
        state.lines = vec!["v0\n".to_string()];
        push_undo(&mut state);
        state.lines = vec!["v1\n".to_string()];
        push_undo(&mut state);
        state.lines = vec!["v2\n".to_string()];

        let blob = serialize_history(&state.undo, &state.redo);
        let (rt_undo, rt_redo) = deserialize_history(&blob).expect("deserialize failed");

        // Replace the live history with the round-tripped version and undo through it.
        state.undo = rt_undo;
        state.redo = rt_redo;
        undo(&mut state);
        assert_eq!(state.lines, vec!["v1\n"]);
        undo(&mut state);
        assert_eq!(state.lines, vec!["v0\n"]);
    }

    #[test]
    fn content_signature_cached_reuses_hash_when_change_id_unchanged() {
        let mut state = default_buffer_state();
        state.lines = vec!["abc\n".to_string()];
        state.change_id = 7;
        state.sig_cache = (0, 0);
        let first = content_signature_cached(&mut state);
        // Cache now keyed on change_id 7. Mutate lines without bumping change_id
        // and confirm the cached hash is returned (the cache is the source of truth
        // until change_id advances, so callers must bump it on edits).
        state.lines = vec!["xyz\n".to_string()];
        let second = content_signature_cached(&mut state);
        assert_eq!(first, second);
        // Advancing change_id invalidates the cache.
        state.change_id = 8;
        let third = content_signature_cached(&mut state);
        assert_ne!(third, first);
    }

    #[test]
    fn push_undo_is_noop_on_huge_buffer() {
        let mut state = default_buffer_state();
        state.lines = vec!["big\n".to_string()];
        state.total_bytes = HUGE_FILE_THRESHOLD + 1;
        let cid_before = state.change_id;
        push_undo(&mut state);
        assert!(state.undo.is_empty(), "huge buffers must not snapshot");
        assert_eq!(
            state.change_id,
            cid_before + 1,
            "change_id must still advance so dirty-check sees the edit"
        );
    }

    #[test]
    fn push_undo_small_buffer_stores_snapshot() {
        let mut state = default_buffer_state();
        state.lines = vec!["small\n".to_string()];
        state.total_bytes = 6;
        push_undo(&mut state);
        assert_eq!(state.undo.len(), 1);
    }

    #[test]
    fn trim_history_to_budget_drops_oldest_first() {
        let mut state = default_buffer_state();
        // Stuff 80 MB of fake undo entries, exceeding the 64 MB budget.
        let entry = vec![0u8; 16 * 1024 * 1024];
        for _ in 0..5 {
            state.undo.push(entry.clone());
        }
        assert_eq!(state.undo.len(), 5);
        trim_history_to_budget(&mut state);
        // 5 × 16 MB = 80 MB. Budget is 64 MB. Should drop one to reach 64 MB.
        assert_eq!(state.undo.len(), 4);
    }

    #[test]
    fn trim_history_always_retains_at_least_one_entry() {
        let mut state = default_buffer_state();
        // A single 100 MB entry is over budget, but must be retained so the
        // most recent edit remains undoable.
        state.undo.push(vec![0u8; 100 * 1024 * 1024]);
        trim_history_to_budget(&mut state);
        assert_eq!(state.undo.len(), 1);
    }

    #[test]
    fn undo_refreshes_total_bytes_after_restore() {
        let mut state = default_buffer_state();
        state.lines = vec!["hello\n".to_string()];
        state.total_bytes = 6;
        push_undo(&mut state);
        state.lines = vec!["hello world\n".to_string()];
        state.total_bytes = 12;
        undo(&mut state);
        assert_eq!(state.lines, vec!["hello\n".to_string()]);
        assert_eq!(state.total_bytes, 6);
    }
}
