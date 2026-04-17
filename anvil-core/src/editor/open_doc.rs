//! Per-tab document state and the session/file I/O helpers that operate
//! on it. Pulled out of `main_loop` so the event loop doesn't host a
//! nested struct + a dozen supporting functions inline.
//!
//! Most functions here take a `use_git: bool` (or similar) argument
//! rather than reaching back into main_loop for the mode, so this
//! module is self-contained and unit-testable.
use std::collections::HashMap;
use std::path::Path;

use crate::editor::buffer;
use crate::editor::doc_view::{DocView, RenderLine};
use crate::editor::git::LineChange;
use crate::editor::main_loop::{normalize_path, AutoreloadState};
use crate::editor::markdown_preview::MarkdownPreviewState;
use crate::editor::picker;
use crate::editor::storage;
use crate::editor::view::View;

/// Everything the editor tracks per open tab: the view state, the path
/// on disk, the saved-state fingerprint for dirty detection, and a few
/// rendering caches.
pub(crate) struct OpenDoc {
    pub view: DocView,
    pub path: String,
    pub name: String,
    pub saved_change_id: i64,
    pub saved_signature: u32,
    pub indent_type: String,
    pub indent_size: usize,
    pub git_changes: HashMap<usize, LineChange>,
    /// Cached tokenized render lines. Invalidated only when the buffer
    /// content changes (edits, undo/redo, reload), NOT on cursor movement.
    pub cached_render: Vec<RenderLine>,
    /// The buffer change_id when cached_render was last built.
    pub cached_change_id: i64,
    /// The scroll-y when cached_render was last built (rebuild on scroll).
    pub cached_scroll_y: f64,
    /// Number of inlay hints when cached_render was last built.
    pub cached_hint_count: usize,
    /// Memoized dirty-check. `(change_id, is_modified)` — valid as long
    /// as the buffer's current change_id matches. Avoids rehashing the
    /// whole buffer 4+ times per render frame for tab labels and status.
    pub dirty_cache: std::cell::Cell<Option<(i64, bool)>>,
    /// Rendered markdown preview state. Idle (zero-cost) until the user
    /// toggles preview on for this tab.
    pub preview: MarkdownPreviewState,
}

/// Byte threshold above which `doc_is_modified` short-circuits to a pure
/// change-id comparison, skipping the `content_signature` fallback that
/// would otherwise scan the whole buffer. Below this size, the signature
/// fallback still runs so "edit then undo back to saved" correctly clears
/// the dirty flag; above it, that niche optimization is sacrificed for
/// responsiveness on multi-GB files.
const DIRTY_STRICT_FALLBACK_BYTES: u64 = 8 * 1024 * 1024;

/// Files larger than this threshold load on a background thread with a
/// progress overlay instead of blocking the UI.
pub(crate) const BG_LOAD_THRESHOLD: u64 = 25 * 1024 * 1024;

/// Session data for save/restore.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct SessionData {
    pub files: Vec<String>,
    pub active: usize,
    #[serde(default)]
    pub active_project: String,
    #[serde(default)]
    pub unsaved_content: Vec<String>,
}

/// Check if a document has unsaved modifications.
///
/// - Fast path: `change_id == saved_change_id` → clean (O(1)).
/// - Small-buffer path: compare cached content signature against the
///   saved one; catches "undo back to saved state".
/// - Huge-buffer path: any change_id mismatch is treated as modified.
///
/// The per-doc `dirty_cache` memoizes the answer for the current
/// change_id so tab-bar and status-bar rendering only pay the cost once.
pub(crate) fn doc_is_modified(doc: &OpenDoc) -> bool {
    let Some(buf_id) = doc.view.buffer_id else {
        return false;
    };
    buffer::with_buffer_mut(buf_id, |b| {
        if b.change_id == doc.saved_change_id {
            doc.dirty_cache.set(Some((b.change_id, false)));
            return Ok(false);
        }
        if let Some((cid, result)) = doc.dirty_cache.get() {
            if cid == b.change_id {
                return Ok(result);
            }
        }
        if b.total_bytes > DIRTY_STRICT_FALLBACK_BYTES {
            doc.dirty_cache.set(Some((b.change_id, true)));
            return Ok(true);
        }
        let modified = buffer::content_signature_cached(b) != doc.saved_signature;
        doc.dirty_cache.set(Some((b.change_id, modified)));
        Ok(modified)
    })
    .unwrap_or(false)
}

/// Builds the "X has unsaved changes, quit anyway?" prompt. If more than
/// one modified doc exists, the subject becomes "Multiple files".
pub(crate) fn nag_msg_quit(docs: &[OpenDoc]) -> String {
    let modified: Vec<&OpenDoc> = docs.iter().filter(|d| doc_is_modified(d)).collect();
    let label = if modified.len() == 1 {
        let name = &modified[0].name;
        if name.is_empty() {
            "untitled".to_string()
        } else {
            name.clone()
        }
    } else {
        "Multiple files".to_string()
    };
    format!("{label} has unsaved changes, quit anyway?")
}

/// Builds the "X has unsaved changes, close anyway?" prompt for a single
/// tab. Always shows the filename, never collapses to "Multiple files".
pub(crate) fn nag_msg_close(name: &str) -> String {
    let label = if name.is_empty() { "untitled" } else { name };
    format!("{label} has unsaved changes, close anyway?")
}

/// Check file size against hard limit. Returns Err with a message if the
/// file exceeds the limit.
pub(crate) fn check_file_size_limit(path: &str, hard_limit_mb: u32) -> Result<u64, String> {
    let sz = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let limit_bytes = (hard_limit_mb as u64) * 1024 * 1024;
    if sz > limit_bytes {
        Err(format!(
            "File too large: {:.1} MB exceeds hard limit of {} MB (set large_file.hard_limit_mb in config.toml)",
            sz as f64 / (1024.0 * 1024.0),
            hard_limit_mb
        ))
    } else {
        Ok(sz)
    }
}

/// Open a file and add it to the docs list. `use_git` controls whether
/// per-line git status is computed at load time.
pub(crate) fn open_file_into(path: &str, docs: &mut Vec<OpenDoc>, use_git: bool) -> bool {
    // Resolve to an absolute path so doc.path round-trips through session
    // save/load even if the cwd changes between runs. `std::path::absolute`
    // does NOT touch the filesystem (preserves symlinks, works for missing
    // files), unlike fs::canonicalize. Falls back to normalize_path on the
    // rare error case so the error message is still meaningful.
    let resolved = std::path::absolute(path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| normalize_path(path));
    let path = resolved.as_str();
    let mut buf_state = buffer::default_buffer_state();
    if let Err(e) = buffer::load_file(&mut buf_state, path) {
        eprintln!("Failed to open {path}: {e}");
        return false;
    }
    let initial_change_id = buf_state.change_id;
    let (indent_type, indent_size, _score) = picker::detect_indent(&buf_state.lines, 100, 2);
    let buf_id = buffer::insert_buffer(buf_state);
    let mut dv = DocView::new();
    dv.buffer_id = Some(buf_id);
    dv.indent_size = indent_size;
    let name = std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string());
    let git_changes = if use_git {
        crate::editor::git::diff_file(path)
    } else {
        HashMap::new()
    };
    let saved_sig =
        buffer::with_buffer(buf_id, |b| Ok(buffer::content_signature(&b.lines))).unwrap_or(0);
    docs.push(OpenDoc {
        view: dv,
        path: path.to_string(),
        name,
        saved_change_id: initial_change_id,
        saved_signature: saved_sig,
        indent_type: indent_type.to_string(),
        indent_size,
        git_changes,
        cached_render: Vec::new(),
        cached_change_id: -1,
        cached_scroll_y: -1.0,
        cached_hint_count: 0,
        dirty_cache: std::cell::Cell::new(None),
        preview: MarkdownPreviewState::default(),
    });
    true
}

/// Derive a storage-safe key from a project root path.
pub(crate) fn project_session_key(root: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let canonical = std::fs::canonicalize(root)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| root.to_string());
    let mut h = DefaultHasher::new();
    canonical.hash(&mut h);
    format!("proj_{:016x}", h.finish())
}

/// Save the current open files for a project so they can be restored later.
pub(crate) fn save_project_session(
    userdir: &Path,
    root: &str,
    docs: &[OpenDoc],
    active_tab: usize,
) {
    if root == "." || root.is_empty() {
        return;
    }
    let mut files = Vec::new();
    let mut unsaved_content = Vec::new();
    for doc in docs {
        if doc.path.is_empty() {
            files.push("__untitled__".to_string());
            let content = doc
                .view
                .buffer_id
                .and_then(|id| buffer::with_buffer(id, |b| Ok(b.lines.join(""))).ok())
                .unwrap_or_default();
            unsaved_content.push(content);
        } else {
            files.push(doc.path.clone());
            unsaved_content.push(String::new());
        }
    }
    let session = SessionData {
        files,
        active: active_tab,
        active_project: root.to_string(),
        unsaved_content,
    };
    if let Ok(json) = serde_json::to_string_pretty(&session) {
        let _ = storage::save_text(
            userdir,
            "project_session",
            &project_session_key(root),
            &json,
        );
    }
}

/// Restore previously saved open files for a project. Returns the active
/// tab index if files were restored. `use_git` is forwarded to
/// `open_file_into` for any non-untitled files.
pub(crate) fn restore_project_session(
    userdir: &Path,
    root: &str,
    docs: &mut Vec<OpenDoc>,
    autoreload: &mut AutoreloadState,
    use_git: bool,
) -> Option<usize> {
    let key = project_session_key(root);
    let data = storage::load_text(userdir, "project_session", &key).ok()??;
    let session: SessionData = serde_json::from_str(&data).ok()?;
    for (i, file) in session.files.iter().enumerate() {
        if file == "__untitled__" {
            let buf_id = buffer::insert_buffer(buffer::default_buffer_state());
            if let Some(content) = session.unsaved_content.get(i) {
                if !content.is_empty() {
                    let _ = buffer::with_buffer_mut(buf_id, |b| {
                        b.lines = content.lines().map(|l| format!("{l}\n")).collect();
                        if b.lines.is_empty() {
                            b.lines.push("\n".to_string());
                        }
                        b.change_id += 1;
                        Ok(())
                    });
                }
            }
            let mut dv = DocView::new();
            dv.buffer_id = Some(buf_id);
            docs.push(OpenDoc {
                view: dv,
                path: String::new(),
                name: "untitled".to_string(),
                saved_change_id: 1,
                saved_signature: buffer::content_signature(&["\n".to_string()]),
                indent_type: "soft".to_string(),
                indent_size: 2,
                git_changes: HashMap::new(),
                cached_render: Vec::new(),
                cached_change_id: -1,
                cached_scroll_y: -1.0,
                cached_hint_count: 0,
                dirty_cache: std::cell::Cell::new(None),
                preview: MarkdownPreviewState::default(),
            });
        } else if open_file_into(file, docs, use_git) {
            autoreload.watch(file);
        }
    }
    if docs.is_empty() {
        None
    } else {
        Some(session.active.min(docs.len().saturating_sub(1)))
    }
}

/// Split `path:N` into `(path, Some(N))`. Handles Windows drive letters
/// (e.g. `C:\foo`) by only treating the trailing `:digits` as a line number.
pub(crate) fn split_path_line(input: &str) -> (&str, Option<usize>) {
    if let Some(pos) = input.rfind(':') {
        let suffix = &input[pos + 1..];
        if !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()) && pos > 0 {
            if let Ok(n) = suffix.parse::<usize>() {
                return (&input[..pos], Some(n));
            }
        }
    }
    (input, None)
}

/// After `open_file_into` pushes a doc, scroll it to `line`.
pub(crate) fn scroll_new_doc_to_line(docs: &mut [OpenDoc], line: usize, style_line_h: f64) {
    if let Some(doc) = docs.last_mut() {
        if let Some(buf_id) = doc.view.buffer_id {
            let _ = buffer::with_buffer_mut(buf_id, |b| {
                let ln = line.min(b.lines.len()).max(1);
                b.selections = vec![ln, 1, ln, 1];
                Ok(())
            });
            let y = ((line as f64 - 1.0) * style_line_h - doc.view.rect().h / 2.0).max(0.0);
            doc.view.scroll_y = y;
            doc.view.target_scroll_y = y;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_path_line_with_number() {
        assert_eq!(split_path_line("foo.rs:42"), ("foo.rs", Some(42)));
    }

    #[test]
    fn split_path_line_no_number() {
        assert_eq!(split_path_line("foo.rs"), ("foo.rs", None));
    }

    #[test]
    fn split_path_line_windows_drive() {
        assert_eq!(split_path_line(r"C:\foo\bar.rs"), (r"C:\foo\bar.rs", None));
    }

    #[test]
    fn split_path_line_windows_drive_with_linenum() {
        assert_eq!(
            split_path_line(r"C:\foo\bar.rs:42"),
            (r"C:\foo\bar.rs", Some(42))
        );
    }

    #[test]
    fn split_path_line_rejects_bare_colon() {
        assert_eq!(split_path_line(":42"), (":42", None));
    }

    #[test]
    fn nag_msg_close_empty_name() {
        assert!(nag_msg_close("").contains("untitled"));
    }

    #[test]
    fn nag_msg_close_with_name() {
        assert_eq!(
            nag_msg_close("main.rs"),
            "main.rs has unsaved changes, close anyway?"
        );
    }

    #[test]
    fn check_file_size_limit_rejects_too_large() {
        let tmp = std::env::temp_dir().join("liteanvil_test_open_doc_size.txt");
        std::fs::write(&tmp, vec![0u8; 2 * 1024 * 1024]).unwrap();
        let result = check_file_size_limit(tmp.to_str().unwrap(), 1);
        assert!(result.is_err());
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn check_file_size_limit_accepts_small() {
        let tmp = std::env::temp_dir().join("liteanvil_test_open_doc_size_small.txt");
        std::fs::write(&tmp, b"hi").unwrap();
        let result = check_file_size_limit(tmp.to_str().unwrap(), 1);
        assert!(result.is_ok());
        let _ = std::fs::remove_file(&tmp);
    }
}
