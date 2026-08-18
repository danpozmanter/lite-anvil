//! Per-tab document state and the session/file I/O helpers that operate
//! on it. Pulled out of `main_loop` so the event loop doesn't host a
//! nested struct + a dozen supporting functions inline.
//!
//! Most functions here take a `use_git: bool` (or similar) argument
//! rather than reaching back into main_loop for the mode, so this
//! module is self-contained and unit-testable.
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use crate::editor::buffer;
use crate::editor::doc_view::{DocView, RenderLine};
use crate::editor::git::LineChange;
use crate::editor::main_loop::{AutoreloadState, normalize_path};
use crate::editor::markdown_preview::MarkdownPreviewState;
use crate::editor::picker;
use crate::editor::storage;
use crate::editor::tokenizer::Token;
use crate::editor::view::View;

/// One line's cached tokenize result, self-describing enough to be
/// validated against the buffer: an entry stamped with the buffer's
/// current `change_id` is trusted as-is; an older entry is reusable only
/// while the line's content hash and the tokenizer state entering the
/// line both match what it was built from. Tokenization is deterministic
/// over `(content, start_state)`, so a matching entry is always correct
/// no matter what edits happened elsewhere in the buffer.
pub(crate) struct CachedLine {
    /// Buffer `change_id` when the entry was created or last validated.
    pub change_id: i64,
    /// FNV-1a fingerprint of the line's raw content (see [`line_hash`]).
    pub content_hash: u64,
    /// Tokenizer state entering the line. The byte stack mirrors the
    /// legacy lite-xl format: each level holds a 1-based pattern index
    /// for a pair pattern still open at that nesting depth (e.g. an
    /// unterminated `/* …`). Empty means outside any multi-line
    /// construct.
    pub start_state: Vec<u8>,
    /// Present while this line remains in the viewport token cache.
    pub tokens: Option<std::sync::Arc<Vec<Token>>>,
    /// Tokenizer state at the end of the line, threaded into the next
    /// line so block comments and other paired constructs span line
    /// boundaries.
    pub end_state: Vec<u8>,
}

/// FNV-1a 64-bit content fingerprint for cached-line validation.
pub(crate) fn line_hash(text: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in text.as_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Per-buffer tokenizer state and viewport-token cache.
///
/// `content_hashes` finds the first changed line without relying on every edit
/// command to maintain a dirty watermark. Sparse `checkpoints` reconstruct the
/// state before a viewport without retaining one cache allocation per source
/// line. `lines` holds only token-heavy entries near recently visible ranges.
pub(crate) struct TokenCache {
    pub lines: HashMap<usize, CachedLine>,
    pub content_hashes: Vec<u64>,
    /// Tokenizer end-state after a 1-based source line.
    pub checkpoints: BTreeMap<usize, Vec<u8>>,
    /// Furthest line reached by the current time-budgeted forward walk.
    pub frontier_line: usize,
    pub frontier_state: Vec<u8>,
    pub change_id: i64,
    /// True when a cold/deep syntax walk yielded and another frame is needed.
    pub pending: bool,
    /// True when a line in the visible range was too long to tokenize within
    /// one frame and was rendered as plain text. Surfaced in the status bar so
    /// missing highlighting reads as a deliberate limit.
    pub syntax_limited: bool,
}

impl Default for TokenCache {
    fn default() -> Self {
        Self {
            lines: HashMap::new(),
            content_hashes: Vec::new(),
            checkpoints: BTreeMap::new(),
            frontier_line: 0,
            frontier_state: Vec::new(),
            change_id: -1,
            pending: false,
            syntax_limited: false,
        }
    }
}

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
    /// Wrapped in `Arc` so cache-hit redraws can share by refcount
    /// instead of cloning the whole `Vec<RenderLine>` each frame.
    pub cached_render: std::sync::Arc<Vec<RenderLine>>,
    /// The buffer change_id when cached_render was last built.
    pub cached_change_id: i64,
    /// The scroll-y when cached_render was last built (rebuild on scroll).
    pub cached_scroll_y: f64,
    /// Number of inlay hints when cached_render was last built.
    pub cached_hint_count: usize,
    /// View width when cached_render was last built (rebuild on resize).
    pub cached_rect_w: f64,
    /// View height when cached_render was last built (rebuild on resize).
    pub cached_rect_h: f64,
    /// Memoized dirty-check. `(change_id, is_modified)` — valid as long
    /// as the buffer's current change_id matches. Avoids rehashing the
    /// whole buffer 4+ times per render frame for tab labels and status.
    pub dirty_cache: std::cell::Cell<Option<(i64, bool)>>,
    /// Per-line tokenize cache. Reused across frames so scrolling does
    /// not re-tokenize lines whose content is unchanged.
    pub token_cache: std::cell::RefCell<TokenCache>,
    /// Rendered markdown preview state. Idle (zero-cost) until the user
    /// toggles preview on for this tab.
    pub preview: MarkdownPreviewState,
    /// `path` as it was when the canonical form beside it was resolved.
    /// Recomputed whenever `path` no longer matches, so a rename or save-as
    /// cannot leave a stale mapping behind.
    pub(crate) canonical_cache: std::cell::RefCell<(String, String)>,
}

/// The document's path with symlinks and `..` resolved, which is how a
/// filesystem event is matched to a tab.
///
/// Memoized against `path`: a watcher burst would otherwise re-canonicalize
/// every open document once per event, and canonicalizing is a syscall.
pub(crate) fn doc_canonical_path(doc: &OpenDoc) -> String {
    {
        let cache = doc.canonical_cache.borrow();
        if cache.0 == doc.path {
            return cache.1.clone();
        }
    }
    let canonical = canonicalize_path(&doc.path);
    *doc.canonical_cache.borrow_mut() = (doc.path.clone(), canonical.clone());
    canonical
}

/// Resolve a path for comparison, falling back to the path itself when it
/// cannot be resolved (it may not exist yet).
pub(crate) fn canonicalize_path(path: &str) -> String {
    std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.to_string())
}

impl Drop for OpenDoc {
    /// Reclaim the backing buffer (its line vector and undo history) from
    /// the global `BUFFERS` map when the tab is closed. Each `OpenDoc`
    /// exclusively owns its `buffer_id` - every id is minted by a fresh
    /// `insert_buffer` and never shared between tabs - so closing the tab
    /// is the point at which the buffer becomes unreachable and should be
    /// freed. Tabs close by `Vec::remove`, by whole-list replacement on
    /// project switch, and at shutdown; routing the reclaim through `Drop`
    /// covers every path without each call site having to remember.
    fn drop(&mut self) {
        if let Some(buf_id) = self.view.buffer_id {
            buffer::remove_buffer(buf_id);
        }
    }
}

/// Byte threshold above which `doc_is_modified` short-circuits to a pure
/// change-id comparison, skipping the `content_signature` fallback that
/// would otherwise scan the whole buffer. Below this size, the signature
/// fallback still runs so "edit then undo back to saved" correctly clears
/// the dirty flag; above it, that niche optimization is sacrificed for
/// responsiveness on multi-GB files.
///
/// Deliberately independent of the large-file soft limit: the signature is
/// recomputed once per edit, so its ceiling is set by what fits in an input
/// frame, not by what the user considers a large file.
const DIRTY_SIGNATURE_SCAN_LIMIT: u64 = 8 * 1024 * 1024;

/// Default threshold above which a file loads on a background thread with a
/// progress overlay instead of blocking the UI. A configured soft limit
/// replaces it; see [`PerformancePolicy`].
pub(crate) const BG_LOAD_THRESHOLD: u64 = 25 * 1024 * 1024;

thread_local! {
    /// Configured large-file settings for this editor thread, installed once
    /// at startup. Every [`PerformancePolicy`] is resolved against it.
    static LARGE_FILE: std::cell::RefCell<crate::editor::config::LargeFileConfig> =
        std::cell::RefCell::new(crate::editor::config::LargeFileConfig::default());
}

/// Install the configured large-file settings for this editor thread.
pub(crate) fn set_large_file_config(cfg: &crate::editor::config::LargeFileConfig) {
    LARGE_FILE.with(|slot| *slot.borrow_mut() = cfg.clone());
}

/// Configured hard limit, consulted on every path that opens a file.
pub(crate) fn hard_limit_mb() -> u32 {
    LARGE_FILE.with(|cfg| cfg.borrow().hard_limit_mb)
}

/// Every size-derived performance decision for one document, resolved in one
/// place from [`crate::editor::config::LargeFileConfig`] and the document's
/// byte count.
///
/// Before this existed the editor consulted four unrelated byte constants and
/// a config field that nothing read, so "large" meant a different size
/// depending on which feature was asking. Anything that keeps its own
/// threshold does so because its rationale is genuinely different, and says
/// so where the threshold is defined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PerformancePolicy {
    /// Document is over the configured soft limit.
    pub large: bool,
    /// Reject mutating commands and explain why.
    pub read_only: bool,
    /// Render without syntax highlighting.
    pub plain_text: bool,
    /// Suppress every language-server interaction for this document.
    pub disable_lsp: bool,
    /// Suppress completion, from both the language server and the document.
    pub disable_autocomplete: bool,
    /// Compare content signatures to answer "is this modified", rather than
    /// trusting the change id alone.
    pub signature_dirty_check: bool,
    /// Longest line that is worth handing to the tokenizer.
    pub syntax_line_limit_bytes: usize,
}

impl PerformancePolicy {
    /// Resolve the policy for a document of `bytes` from an explicit config.
    pub(crate) fn resolve(
        bytes: u64,
        cfg: &crate::editor::config::LargeFileConfig,
    ) -> PerformancePolicy {
        let large = cfg.soft_limit_mb != 0 && bytes > u64::from(cfg.soft_limit_mb) * 1024 * 1024;
        PerformancePolicy {
            large,
            read_only: large && cfg.read_only,
            plain_text: large && cfg.plain_text,
            disable_lsp: large && cfg.disable_lsp,
            disable_autocomplete: large && cfg.disable_autocomplete,
            signature_dirty_check: bytes <= DIRTY_SIGNATURE_SCAN_LIMIT,
            syntax_line_limit_bytes: (cfg.long_line_limit_kb as usize).saturating_mul(1024),
        }
    }

    /// Resolve the policy for a document of `bytes` from the installed config.
    pub(crate) fn for_bytes(bytes: u64) -> PerformancePolicy {
        LARGE_FILE.with(|cfg| PerformancePolicy::resolve(bytes, &cfg.borrow()))
    }

    /// Byte size above which opening a file loads it on a background thread.
    /// The configured soft limit governs when set, so a user who declares a
    /// smaller "large file" size also gets the progress overlay sooner.
    pub(crate) fn background_load_threshold() -> u64 {
        LARGE_FILE.with(|cfg| {
            let soft = cfg.borrow().soft_limit_mb;
            if soft == 0 {
                BG_LOAD_THRESHOLD
            } else {
                u64::from(soft) * 1024 * 1024
            }
        })
    }
}

/// Longest line worth tokenizing, for render paths that see lines rather than
/// documents. One generated or minified line can otherwise consume a whole
/// input frame inside a single regex call that the frame budget cannot
/// preempt, no matter how small the file containing it is.
pub(crate) fn syntax_line_limit_bytes() -> usize {
    LARGE_FILE.with(|cfg| (cfg.borrow().long_line_limit_kb as usize).saturating_mul(1024))
}

/// The performance policy for an open document.
pub(crate) fn doc_policy(doc: &OpenDoc) -> PerformancePolicy {
    PerformancePolicy::for_bytes(doc_bytes(doc))
}

/// Byte size of a document's buffer, or zero if it has none.
pub(crate) fn doc_bytes(doc: &OpenDoc) -> u64 {
    doc.view
        .buffer_id
        .and_then(|id| buffer::with_buffer(id, |b| Ok(b.total_bytes)).ok())
        .unwrap_or(0)
}

/// Heap bytes one open document retains, by what holds them. Covers the
/// allocations that scale with document size and session length; small fixed
/// per-document state is not itemized.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DocMemory {
    pub text: u64,
    pub history: u64,
    pub search_subject: u64,
    pub token_cache: u64,
    pub render_cache: u64,
    pub preview: u64,
}

impl DocMemory {
    pub(crate) fn total(&self) -> u64 {
        self.text
            + self.history
            + self.search_subject
            + self.token_cache
            + self.render_cache
            + self.preview
    }
}

/// Measure what one open document is holding.
pub(crate) fn doc_memory(doc: &OpenDoc) -> DocMemory {
    let (text, history, search_subject) = doc
        .view
        .buffer_id
        .and_then(|id| {
            buffer::with_buffer(id, |b| {
                Ok((b.text_bytes(), b.history_bytes(), b.search_bytes()))
            })
            .ok()
        })
        .unwrap_or((0, 0, 0));

    let cache = doc.token_cache.borrow();
    let token_cache = cache
        .lines
        .values()
        .map(|line| {
            let tokens = line
                .tokens
                .as_ref()
                .map(|tokens| {
                    tokens
                        .iter()
                        .map(|t| {
                            (t.text.capacity()
                                + std::mem::size_of::<crate::editor::tokenizer::Token>())
                                as u64
                        })
                        .sum::<u64>()
                })
                .unwrap_or(0);
            tokens + (line.start_state.capacity() + line.end_state.capacity()) as u64
        })
        .sum::<u64>()
        + (cache.content_hashes.capacity() * std::mem::size_of::<u64>()) as u64
        + cache
            .checkpoints
            .values()
            .map(|state| state.capacity() as u64)
            .sum::<u64>();

    let render_cache = doc
        .cached_render
        .iter()
        .map(|line| {
            line.tokens
                .iter()
                .map(|t| {
                    (t.text.capacity()
                        + std::mem::size_of::<crate::editor::doc_view::RenderToken>())
                        as u64
                })
                .sum::<u64>()
        })
        .sum::<u64>();

    DocMemory {
        text,
        history,
        search_subject,
        token_cache,
        render_cache,
        preview: doc.preview.retained_bytes(),
    }
}

/// What an external change to a document's file should cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExternalChange {
    /// The bytes on disk match what the editor last wrote. We watch the parent
    /// directory, so our own saves - notably notes-mode autosave - come back
    /// as change events; comparing content is what tells them apart.
    Echo,
    /// The document has unsaved edits that adopting the file would discard.
    AskUser,
    /// Safe to adopt the file's contents.
    Adopt,
}

/// Decide what a document should do about the contents now on disk.
pub(crate) fn classify_external_change(doc: &OpenDoc, disk_signature: u32) -> ExternalChange {
    if disk_signature == doc.saved_signature {
        ExternalChange::Echo
    } else if doc_is_modified(doc) {
        ExternalChange::AskUser
    } else {
        ExternalChange::Adopt
    }
}

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
        if !PerformancePolicy::for_bytes(b.total_bytes).signature_dirty_check {
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

fn open_file_into_with_tab_limit(
    path: &str,
    docs: &mut Vec<OpenDoc>,
    use_git: bool,
    enforce_tab_limit: bool,
) -> bool {
    let _perf = crate::editor::perf::span("open_file");
    if enforce_tab_limit && !crate::editor::main_loop::can_open_another_tab(docs.len()) {
        eprintln!("Open tab limit reached");
        return false;
    }
    // Resolve to an absolute path so doc.path round-trips through session
    // save/load even if the cwd changes between runs. `std::path::absolute`
    // does NOT touch the filesystem (preserves symlinks, works for missing
    // files), unlike fs::canonicalize. Falls back to normalize_path on the
    // rare error case so the error message is still meaningful.
    let resolved = std::path::absolute(path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| normalize_path(path));
    let path = resolved.as_str();
    if let Err(message) = check_file_size_limit(path, hard_limit_mb()) {
        eprintln!("{message}");
        return false;
    }
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
    if use_git {
        // The gutter starts empty and is filled when main_loop applies the result
        // from git::drain_diffs, so a large repo never stalls the file open.
        crate::editor::git::start_diff(path);
    }
    let git_changes = HashMap::new();
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
        cached_render: std::sync::Arc::new(Vec::new()),
        cached_change_id: -1,
        cached_scroll_y: -1.0,
        cached_hint_count: 0,
        cached_rect_w: -1.0,
        cached_rect_h: -1.0,
        dirty_cache: std::cell::Cell::new(None),
        token_cache: std::cell::RefCell::new(TokenCache::default()),
        preview: MarkdownPreviewState::default(),
        canonical_cache: Default::default(),
    });
    true
}

/// Open a file and add it to the docs list. `use_git` controls whether
/// per-line git status is computed at load time.
pub(crate) fn open_file_into(path: &str, docs: &mut Vec<OpenDoc>, use_git: bool) -> bool {
    open_file_into_with_tab_limit(path, docs, use_git, true)
}

/// Open a file from an explicit user action, bypassing the bulk-open tab cap.
pub(crate) fn open_file_into_user_requested(
    path: &str,
    docs: &mut Vec<OpenDoc>,
    use_git: bool,
) -> bool {
    open_file_into_with_tab_limit(path, docs, use_git, false)
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
                cached_render: std::sync::Arc::new(Vec::new()),
                cached_change_id: -1,
                cached_scroll_y: -1.0,
                cached_hint_count: 0,
                cached_rect_w: -1.0,
                cached_rect_h: -1.0,
                dirty_cache: std::cell::Cell::new(None),
                token_cache: std::cell::RefCell::new(TokenCache::default()),
                preview: MarkdownPreviewState::default(),
                canonical_cache: Default::default(),
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

    fn large_file_config(soft_limit_mb: u32) -> crate::editor::config::LargeFileConfig {
        crate::editor::config::LargeFileConfig {
            soft_limit_mb,
            ..Default::default()
        }
    }

    #[test]
    fn an_external_change_matching_our_last_save_is_an_echo() {
        let buf_id = buffer::insert_buffer(buffer::default_buffer_state());
        let mut doc = make_doc(buf_id);
        doc.saved_signature = 12345;
        assert_eq!(
            classify_external_change(&doc, 12345),
            ExternalChange::Echo,
            "our own write must not be reported as an external change"
        );
    }

    #[test]
    fn an_external_change_to_an_unmodified_document_is_adopted() {
        let buf_id = buffer::insert_buffer(buffer::default_buffer_state());
        let mut doc = make_doc(buf_id);
        doc.saved_signature = 1;
        doc.saved_change_id = buffer::with_buffer(buf_id, |b| Ok(b.change_id)).unwrap();
        assert_eq!(classify_external_change(&doc, 2), ExternalChange::Adopt);
    }

    #[test]
    fn an_external_change_to_a_modified_document_asks_the_user() {
        let buf_id = buffer::insert_buffer(buffer::default_buffer_state());
        let _ = buffer::with_buffer_mut(buf_id, |b| {
            b.lines = vec!["locally edited\n".to_string()];
            b.change_id += 1;
            Ok(())
        });
        let mut doc = make_doc(buf_id);
        doc.saved_signature = 1;
        doc.saved_change_id = 0;
        assert_eq!(
            classify_external_change(&doc, 2),
            ExternalChange::AskUser,
            "local edits must never be discarded without asking"
        );
    }

    #[test]
    fn doc_memory_accounts_for_the_document_text() {
        let buf_id = buffer::insert_buffer(buffer::default_buffer_state());
        let _ = buffer::with_buffer_mut(buf_id, |b| {
            b.lines = (0..1000).map(|i| format!("line {i}\n")).collect();
            Ok(())
        });
        let doc = make_doc(buf_id);
        let memory = doc_memory(&doc);
        assert!(
            memory.text >= 1000 * 6,
            "text bytes must cover the line contents, got {}",
            memory.text
        );
        assert_eq!(
            memory.total(),
            memory.text
                + memory.history
                + memory.search_subject
                + memory.token_cache
                + memory.render_cache
                + memory.preview
        );
    }

    #[test]
    fn doc_memory_counts_undo_history_separately_from_text() {
        let buf_id = buffer::insert_buffer(buffer::default_buffer_state());
        let _ = buffer::with_buffer_mut(buf_id, |b| {
            b.lines = vec!["original\n".to_string()];
            buffer::push_undo(b);
            b.lines = vec!["replaced\n".to_string()];
            buffer::push_undo(b);
            Ok(())
        });
        let doc = make_doc(buf_id);
        let memory = doc_memory(&doc);
        assert!(
            memory.history > 0,
            "an edited document must report its undo history"
        );
    }

    #[test]
    fn policy_below_the_soft_limit_leaves_every_feature_enabled() {
        let cfg = large_file_config(20);
        let policy = PerformancePolicy::resolve(1024 * 1024, &cfg);
        assert!(!policy.large);
        assert!(!policy.read_only);
        assert!(!policy.plain_text);
        assert!(!policy.disable_lsp);
        assert!(!policy.disable_autocomplete);
    }

    #[test]
    fn policy_above_the_soft_limit_applies_every_configured_restriction() {
        let cfg = large_file_config(20);
        let policy = PerformancePolicy::resolve(21 * 1024 * 1024, &cfg);
        assert!(policy.large);
        assert_eq!(policy.read_only, cfg.read_only);
        assert_eq!(policy.plain_text, cfg.plain_text);
        assert_eq!(policy.disable_lsp, cfg.disable_lsp);
        assert_eq!(policy.disable_autocomplete, cfg.disable_autocomplete);
    }

    #[test]
    fn a_zero_soft_limit_disables_large_file_behavior_entirely() {
        let cfg = large_file_config(0);
        let policy = PerformancePolicy::resolve(4 * 1024 * 1024 * 1024, &cfg);
        assert!(!policy.large);
        assert!(!policy.read_only);
        assert!(!policy.plain_text);
    }

    #[test]
    fn a_restriction_turned_off_in_config_stays_off_above_the_limit() {
        let cfg = crate::editor::config::LargeFileConfig {
            soft_limit_mb: 20,
            read_only: false,
            disable_lsp: false,
            ..Default::default()
        };
        let policy = PerformancePolicy::resolve(21 * 1024 * 1024, &cfg);
        assert!(policy.large);
        assert!(!policy.read_only);
        assert!(!policy.disable_lsp);
    }

    #[test]
    fn the_long_line_limit_is_independent_of_document_size() {
        let cfg = crate::editor::config::LargeFileConfig {
            long_line_limit_kb: 32,
            ..Default::default()
        };
        let small = PerformancePolicy::resolve(1024, &cfg);
        let big = PerformancePolicy::resolve(u64::from(cfg.soft_limit_mb + 1) * 1024 * 1024, &cfg);
        assert_eq!(small.syntax_line_limit_bytes, 32 * 1024);
        assert_eq!(big.syntax_line_limit_bytes, 32 * 1024);
    }

    #[test]
    fn the_signature_dirty_check_is_dropped_only_for_documents_too_big_to_rehash() {
        let cfg = large_file_config(20);
        assert!(PerformancePolicy::resolve(DIRTY_SIGNATURE_SCAN_LIMIT, &cfg).signature_dirty_check);
        assert!(
            !PerformancePolicy::resolve(DIRTY_SIGNATURE_SCAN_LIMIT + 1, &cfg).signature_dirty_check
        );
    }

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

    fn make_doc(buf_id: u64) -> OpenDoc {
        let mut dv = DocView::new();
        dv.buffer_id = Some(buf_id);
        OpenDoc {
            view: dv,
            path: String::new(),
            name: "t".to_string(),
            saved_change_id: 1,
            saved_signature: 0,
            indent_type: "soft".to_string(),
            indent_size: 2,
            git_changes: HashMap::new(),
            cached_render: std::sync::Arc::new(Vec::new()),
            cached_change_id: -1,
            cached_scroll_y: -1.0,
            cached_hint_count: 0,
            cached_rect_w: -1.0,
            cached_rect_h: -1.0,
            dirty_cache: std::cell::Cell::new(None),
            token_cache: std::cell::RefCell::new(TokenCache::default()),
            preview: MarkdownPreviewState::default(),
            canonical_cache: Default::default(),
        }
    }

    #[test]
    fn dropping_doc_frees_its_buffer() {
        let buf_id = buffer::insert_buffer(buffer::default_buffer_state());
        assert!(
            buffer::with_buffer(buf_id, |_| Ok(())).is_ok(),
            "buffer should exist after insert"
        );
        let doc = make_doc(buf_id);
        drop(doc);
        assert!(
            buffer::with_buffer(buf_id, |_| Ok(())).is_err(),
            "buffer must be reclaimed when its OpenDoc is dropped"
        );
    }

    #[test]
    fn removing_doc_from_vec_frees_its_buffer() {
        let buf_id = buffer::insert_buffer(buffer::default_buffer_state());
        let mut docs = vec![make_doc(buf_id)];
        docs.remove(0);
        assert!(
            buffer::with_buffer(buf_id, |_| Ok(())).is_err(),
            "Vec::remove of a tab must reclaim its buffer"
        );
    }
}
