use pcre2::bytes::{Regex, RegexBuilder};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use crate::editor::error::RegexError;
use crate::editor::syntax::{PatternSpec, SubSyntaxSpec, SyntaxDefinition, TokenType};

/// A single token produced by the tokenizer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// Interned name of the token's type. Shared via `Arc` so re-tokenizing a
    /// line reuses one allocation per type across all its tokens.
    pub token_type: Arc<str>,
    pub text: String,
}

// ── UTF-8 char↔byte index ───────────────────────────────────────────────
//
// `tokenize_line_with_state` invokes `ucharpos`, `prefix_ulen`, and `usub`
// many times per line — every pattern attempt at every column, plus every
// match result. The naive implementations walk the bytes from the start on
// each call (O(line_len) each), so a 500-char UTF-8 markdown line with ~90
// patterns paid O(line_len² × patterns) just in index conversion. Measured
// on the project changelog (1200-line markdown, em-dashes throughout):
// ~3.9 ms / line in release.
//
// The fix is a per-line char↔byte index. `tokenize_line_with_state` calls
// `prime_utf8_line_index` once before its inner loop; every helper call
// inside the loop reads the matching cache and runs in O(1). External
// callers of the helpers (none today inside this crate, but the surface
// is `pub`) keep their legacy walk via the fallback path so the public
// semantics are unchanged.

thread_local! {
    static UTF8_LINE_INDEX: RefCell<LineIndex> = const { RefCell::new(LineIndex::EMPTY) };
}

struct LineIndex {
    line_ptr: *const u8,
    line_len: usize,
    is_ascii: bool,
    /// `char_to_byte[k]` = byte offset of the k-th character (0-based).
    /// Final entry equals `line.len()` so the helper can read one past
    /// the last character without a bounds check. Empty when `is_ascii`.
    /// Offsets are stored as `u32`: a single line never exceeds 4 GiB,
    /// which halves the table size on non-ASCII lines versus `usize`.
    char_to_byte: Vec<u32>,
    /// `byte_to_char[b]` = char index whose byte range contains byte `b`.
    /// Length = `line.len() + 1`. Bytes inside a multi-byte sequence map
    /// to the index of the lead char. `byte_to_char[line.len()]` is the
    /// total char count. Empty when `is_ascii`. Stored as `u32` for the
    /// same per-line size bound as `char_to_byte`.
    byte_to_char: Vec<u32>,
}

impl LineIndex {
    const EMPTY: LineIndex = LineIndex {
        line_ptr: std::ptr::null(),
        line_len: 0,
        is_ascii: true,
        char_to_byte: Vec::new(),
        byte_to_char: Vec::new(),
    };
}

/// Build (or refresh) the thread-local char↔byte index for `line`. Cheap
/// no-op when the index already matches this `&str` (by pointer + length).
pub fn prime_utf8_line_index(line: &str) {
    UTF8_LINE_INDEX.with(|cell| {
        let mut idx = cell.borrow_mut();
        if idx.line_ptr == line.as_ptr() && idx.line_len == line.len() {
            return;
        }
        idx.line_ptr = line.as_ptr();
        idx.line_len = line.len();
        idx.is_ascii = line.is_ascii();
        idx.char_to_byte.clear();
        idx.byte_to_char.clear();
        // A single outsized line (e.g. a minified one-line file) would otherwise
        // pin its peak capacity for the thread's lifetime; release it back
        // toward a bounded reserve so steady-state lines keep the resident set
        // small. `shrink_to` never drops below what the next line needs.
        const LINE_INDEX_RETAIN: usize = 64 * 1024;
        let want = (line.len() + 1).max(LINE_INDEX_RETAIN);
        if idx.byte_to_char.capacity() > want {
            idx.byte_to_char.shrink_to(want);
        }
        if idx.char_to_byte.capacity() > want {
            idx.char_to_byte.shrink_to(want);
        }
        if idx.is_ascii {
            // ASCII path skips the arrays entirely — byte index equals
            // char index, no table needed.
            return;
        }
        idx.byte_to_char.resize(line.len() + 1, 0);
        let mut prev_byte = 0usize;
        let mut char_count = 0usize;
        for (char_idx, (byte_off, _)) in line.char_indices().enumerate() {
            idx.char_to_byte.push(byte_off as u32);
            // Fill the bytes belonging to the *previous* char (mid-sequence
            // bytes for multi-byte UTF-8) with that char's index.
            for b in prev_byte..byte_off {
                idx.byte_to_char[b] = char_idx.saturating_sub(1) as u32;
            }
            prev_byte = byte_off;
            char_count = char_idx + 1;
        }
        idx.char_to_byte.push(line.len() as u32);
        for b in prev_byte..line.len() {
            idx.byte_to_char[b] = char_count.saturating_sub(1) as u32;
        }
        idx.byte_to_char[line.len()] = char_count as u32;
    });
}

fn with_line_index<R>(text: &str, f: impl FnOnce(&LineIndex) -> R) -> Option<R> {
    UTF8_LINE_INDEX.with(|cell| {
        let idx = cell.borrow();
        if idx.line_ptr == text.as_ptr() && idx.line_len == text.len() {
            Some(f(&idx))
        } else {
            None
        }
    })
}

/// Count UTF-8 characters in a string. O(1) for ASCII; O(1) when the
/// thread-local line index matches; otherwise O(N) via `chars().count()`.
pub fn char_len(text: &str) -> usize {
    if text.is_ascii() {
        return text.len();
    }
    if let Some(n) = with_line_index(text, |idx| {
        if idx.is_ascii {
            text.len()
        } else {
            // `char_to_byte` length is char_count + 1 (final = text.len()).
            idx.char_to_byte.len().saturating_sub(1)
        }
    }) {
        return n;
    }
    text.chars().count()
}

/// 1-based character substring (inclusive on both ends).
pub fn usub(text: &str, start: usize, end: usize) -> &str {
    if start == 0 || start > end {
        return "";
    }
    if let Some(s) = with_line_index(text, |idx| {
        if idx.is_ascii {
            let s_byte = (start - 1).min(text.len());
            let e_byte = end.min(text.len());
            if s_byte >= e_byte {
                return "";
            }
            return &text[s_byte..e_byte];
        }
        let max_char = idx.char_to_byte.len().saturating_sub(1);
        let s_idx = (start - 1).min(max_char);
        let e_idx = end.min(max_char);
        let s_byte = idx.char_to_byte[s_idx] as usize;
        let e_byte = idx.char_to_byte[e_idx] as usize;
        if s_byte >= e_byte {
            return "";
        }
        &text[s_byte..e_byte]
    }) {
        return s;
    }
    if text.is_ascii() {
        let s_byte = (start - 1).min(text.len());
        let e_byte = end.min(text.len());
        if s_byte >= e_byte {
            return "";
        }
        return &text[s_byte..e_byte];
    }
    // Legacy walk for un-primed UTF-8 callers.
    let mut start_byte = None;
    let mut end_byte = None;
    for (idx, (byte_idx, _)) in (1usize..).zip(text.char_indices()) {
        if idx == start {
            start_byte = Some(byte_idx);
        }
        if idx == end + 1 {
            end_byte = Some(byte_idx);
            break;
        }
    }
    let Some(start_byte) = start_byte else {
        return "";
    };
    let end_byte = end_byte.unwrap_or(text.len());
    &text[start_byte..end_byte]
}

/// Count UTF-8 characters in the first `byte_count` bytes of `text`.
pub fn prefix_ulen(text: &str, byte_count: usize) -> usize {
    let clamped = byte_count.min(text.len());
    if let Some(n) = with_line_index(text, |idx| {
        if idx.is_ascii {
            clamped
        } else {
            idx.byte_to_char[clamped] as usize
        }
    }) {
        return n;
    }
    if text.is_ascii() {
        return clamped;
    }
    let mut end = clamped;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].chars().count()
}

/// 1-based byte position of the `char_idx`-th character.
pub fn ucharpos(text: &str, char_idx: usize) -> Option<usize> {
    if char_idx == 0 {
        return Some(1);
    }
    if let Some(r) = with_line_index(text, |idx| {
        if idx.is_ascii {
            if char_idx > text.len() {
                return None;
            }
            return Some(char_idx);
        }
        let max_char = idx.char_to_byte.len().saturating_sub(1);
        if char_idx > max_char {
            return None;
        }
        Some(idx.char_to_byte[char_idx - 1] as usize + 1)
    }) {
        return r;
    }
    if text.is_ascii() {
        if char_idx > text.len() {
            return None;
        }
        return Some(char_idx);
    }
    let mut count = 0usize;
    for (byte_idx, _) in text.char_indices() {
        count += 1;
        if count == char_idx {
            return Some(byte_idx + 1);
        }
    }
    None
}

/// Strip a leading `^` anchor and return the code and whether it was anchored.
pub fn split_anchor(code: String) -> (String, bool) {
    if let Some(stripped) = code.strip_prefix('^') {
        (stripped.to_string(), true)
    } else {
        (code, false)
    }
}

/// Get the first byte of a string.
pub fn first_byte(value: &str) -> Option<u8> {
    value.as_bytes().first().copied()
}

thread_local! {
    /// Interns the small, fixed set of token-type names so re-tokenizing a line
    /// shares one heap allocation per type rather than allocating a fresh string
    /// for every token. The set is bounded by the grammar's declared token
    /// types, so the table cannot grow without bound.
    static TOKEN_TYPE_INTERN: std::cell::RefCell<HashMap<Box<str>, Arc<str>>> =
        std::cell::RefCell::new(HashMap::new());
}

/// Return a shared `Arc<str>` for a token-type name, interning on first use.
pub fn intern_token_type(name: &str) -> Arc<str> {
    TOKEN_TYPE_INTERN.with(|cell| {
        let mut map = cell.borrow_mut();
        if let Some(existing) = map.get(name) {
            return Arc::clone(existing);
        }
        let interned: Arc<str> = Arc::from(name);
        map.insert(Box::from(name), Arc::clone(&interned));
        interned
    })
}

/// Append a token to a list, merging with the previous token if types match
/// or the previous token is pure whitespace.
pub fn push_token(tokens: &mut Vec<Token>, token_type: &str, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(prev) = tokens.last_mut() {
        if prev.token_type.as_ref() == token_type
            || (prev.text.chars().all(char::is_whitespace) && token_type != "incomplete")
        {
            prev.token_type = intern_token_type(token_type);
            prev.text.push_str(text);
            return;
        }
    }
    tokens.push(Token {
        token_type: intern_token_type(token_type),
        text: text.to_string(),
    });
}

/// Append tokens from a pattern match, splitting on captures if present.
pub fn push_tokens(
    tokens: &mut Vec<Token>,
    symbols: &HashMap<String, String>,
    token_types: &[String],
    full_text: &str,
    mut find_results: Vec<usize>,
) {
    if find_results.len() > 2 {
        find_results.insert(2, find_results[0]);
        let end_copy = find_results[1] + 1;
        find_results.push(end_copy);
        for i in 2..find_results.len() - 1 {
            let start = find_results[i];
            let fin = find_results[i + 1].saturating_sub(1);
            if fin >= start {
                let text = usub(full_text, start, fin);
                let token_type = token_types
                    .get(i - 2)
                    .map(String::as_str)
                    .unwrap_or_else(|| token_types.first().map(String::as_str).unwrap_or("normal"));
                let mapped = symbols.get(text).map(String::as_str).unwrap_or(token_type);
                push_token(tokens, mapped, text);
            }
        }
    } else if find_results.len() >= 2 {
        let start = find_results[0];
        let fin = find_results[1];
        let text = usub(full_text, start, fin);
        let token_type = token_types.first().map(String::as_str).unwrap_or("normal");
        let mapped = symbols.get(text).map(String::as_str).unwrap_or(token_type);
        push_token(tokens, mapped, text);
    }
}

// ── Compiled syntax types ────────────────────────────────────────────────────

/// 256-bit bitset indexed by raw byte value. `None` means "any byte" —
/// the runtime must always attempt the regex match. `Some(set)` means the
/// regex can only match anchored at a position whose byte is in `set`, so
/// the inner tokenize loop can skip the (anchored) match entirely whenever
/// the current byte isn't a member.
///
/// The set is a *superset* of the bytes the regex can actually match; any
/// uncertainty in the analyzer (groups, lookarounds, Unicode-aware classes,
/// inline flag modifiers, quantifiers on the first element, …) collapses
/// to `None`. False positives (bytes in the set that the regex can't
/// actually match) are fine — the runtime just wastes one anchored match.
/// False negatives (a byte missing from the set that the regex *can*
/// match) would be a correctness bug, which is why the analyzer is
/// conservative.
type FirstByteSet = [u64; 4];

#[inline]
fn fbs_new() -> FirstByteSet {
    [0u64; 4]
}

#[inline]
fn fbs_set(set: &mut FirstByteSet, b: u8) {
    set[(b >> 6) as usize] |= 1u64 << (b & 63);
}

#[inline]
fn fbs_contains(set: &FirstByteSet, b: u8) -> bool {
    set[(b >> 6) as usize] & (1u64 << (b & 63)) != 0
}

/// Conservatively compute the first-byte set of `pattern` (a PCRE2 regex
/// source string). Returns `None` if the analyzer can't prove a tighter
/// set than "any byte" — the caller then falls back to always trying the
/// regex match.
///
/// Handled (safe to filter):
/// - Leading anchors `^`, `\A`, ``, `\B`, `\z`, `\Z` (skip and continue).
/// - Literal ASCII chars not in the metachar set.
/// - Literal non-ASCII chars (record their UTF-8 lead byte).
/// - Escaped punctuation `\.`, `\*`, `\(`, …
/// - Bracket char classes `[abc]`, `[a-z]`, with `\.`-style escapes inside.
///
/// Bails (returns None):
/// - Empty pattern, lone `.`, lone `\d`/`\w`/`\s` (UCP-aware in our builder).
/// - Negated classes `[^…]` and negated escapes `\D`, `\W`, `\S`.
/// - Any kind of group `(...)`, `(?:...)`, `(?=...)`, `(?<=...)`.
/// - Inline flag modifiers `(?i)`, `(?m)`, …
/// - Backreferences ``..`\9`.
/// - Quantifiers `?`, `*`, `{0,…}` on the first element (the rest could be
///   the first non-zero-width match).
/// - Anything else the parser doesn't explicitly recognise.
fn analyze_first_byte_set(pattern: &str) -> Option<FirstByteSet> {
    let bytes = pattern.as_bytes();
    // Bail on any top-level alternation: `a|b` could match either branch,
    // and an empty branch (`a|` / `|a`) makes the regex match zero-width
    // at every position. Walking the structure to enumerate branch first
    // bytes is doable but error-prone; refusing top-level `|` is both
    // simpler and demonstrably safe.
    if has_top_level_alternation(bytes) {
        return None;
    }
    let mut i = 0;
    // Skip leading anchors and zero-width assertions.
    while i < bytes.len() {
        match bytes[i] {
            b'^' => i += 1,
            b'\\' if i + 1 < bytes.len() => match bytes[i + 1] {
                b'A' | b'b' | b'B' | b'z' | b'Z' => i += 2,
                _ => break,
            },
            _ => break,
        }
    }
    if i >= bytes.len() {
        return None;
    }
    // Reject quantifier *immediately after* the first element. The first
    // element could be zero-width, so the second element could be the
    // actual first match — too complex to follow safely.
    let first_element_end = first_element_end(bytes, i)?;
    if let Some(&q) = bytes.get(first_element_end) {
        if q == b'?' || q == b'*' || q == b'{' {
            return None;
        }
    }
    analyze_element(bytes, i)
}

/// True if the regex source contains a `|` at depth 0 outside any
/// character class. Used to bail the analyzer: alternation lets the regex
/// match starting with any branch's first set (including empty), which is
/// more than the first-element analyzer would catch.
fn has_top_level_alternation(bytes: &[u8]) -> bool {
    let mut depth = 0i32;
    let mut in_class = false;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\\' && i + 1 < bytes.len() {
            i += 2;
            continue;
        }
        if in_class {
            if c == b']' {
                in_class = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'[' => in_class = true,
            b'(' => depth += 1,
            b')' => depth = (depth - 1).max(0),
            b'|' if depth == 0 => return true,
            _ => {}
        }
        i += 1;
    }
    false
}

/// Walk one regex element starting at `i`. On uncertainty returns None.
fn analyze_element(bytes: &[u8], i: usize) -> Option<FirstByteSet> {
    let c = *bytes.get(i)?;
    match c {
        b'.' => None,
        b'(' => None, // groups — bail
        b'[' => analyze_char_class(bytes, i),
        b'\\' => {
            let esc = *bytes.get(i + 1)?;
            analyze_escape(esc)
        }
        // Metachars that shouldn't appear as the first element on their own.
        b'$' | b'|' | b'+' | b'?' | b'*' | b'{' | b'}' | b']' | b')' => None,
        _ => {
            // Literal byte. For multi-byte UTF-8, this is the lead byte of
            // a sequence (subsequent bytes will be 0x80+ continuation
            // bytes, which can't start a UTF-8 codepoint, so they wouldn't
            // be checked as `i` positions anyway).
            let mut set = fbs_new();
            fbs_set(&mut set, c);
            Some(set)
        }
    }
}

/// Length-in-bytes of the first regex element starting at byte offset `i`.
/// Used purely to peek at the byte AFTER the first element so we can detect
/// `?`/`*`/`{` quantifiers. Returns None if structure is unclear.
fn first_element_end(bytes: &[u8], i: usize) -> Option<usize> {
    let c = *bytes.get(i)?;
    match c {
        b'.' => Some(i + 1),
        b'\\' if i + 1 < bytes.len() => Some(i + 2),
        b'[' => {
            // Scan until matching ']', respecting escapes.
            let mut j = i + 1;
            // PCRE2 allows `]` as the first char to be literal.
            if matches!(bytes.get(j), Some(b'^')) {
                j += 1;
            }
            if matches!(bytes.get(j), Some(b']')) {
                j += 1;
            }
            while j < bytes.len() {
                match bytes[j] {
                    b'\\' => j += 2,
                    b']' => return Some(j + 1),
                    _ => j += 1,
                }
            }
            None
        }
        _ if c == b'(' || c == b')' => None,
        _ => Some(i + 1),
    }
}

fn analyze_escape(esc: u8) -> Option<FirstByteSet> {
    // Unicode/character-class escapes: too broad in our `ucp(true)` mode.
    // `\d` matches Arabic-Indic digits and other Unicode digits; `\w`
    // matches Unicode word chars; `\s` matches Unicode whitespace.
    if matches!(
        esc,
        b'd' | b'D' | b'w' | b'W' | b's' | b'S' | b'h' | b'H' | b'v' | b'V' | b'p' | b'P' | b'X'
    ) {
        return None;
    }
    // Backreferences: ..\9.
    if esc.is_ascii_digit() {
        return None;
    }
    // Unknown alphabetic escape: bail.
    if esc.is_ascii_alphabetic() {
        return None;
    }
    // Punctuation escape — the literal char itself.
    let mut set = fbs_new();
    fbs_set(&mut set, esc);
    Some(set)
}

fn analyze_char_class(bytes: &[u8], start: usize) -> Option<FirstByteSet> {
    let mut i = start + 1;
    if matches!(bytes.get(i), Some(b'^')) {
        return None; // Negated class — too broad.
    }
    let mut set = fbs_new();
    // PCRE2: a `]` immediately after `[` (or `[^`) is a literal `]`.
    let mut first_in_class = true;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b']' && !first_in_class {
            return Some(set);
        }
        first_in_class = false;
        if c == b'\\' {
            let esc = *bytes.get(i + 1)?;
            // Reject Unicode-aware classes inside; safest path.
            if matches!(
                esc,
                b'd' | b'D' | b'w' | b'W' | b's' | b'S' | b'h' | b'H' | b'v' | b'V' | b'p' | b'P'
            ) {
                return None;
            }
            fbs_set(&mut set, esc);
            i += 2;
            continue;
        }
        // Look for a range `a-z`.
        if i + 2 < bytes.len() && bytes[i + 1] == b'-' && bytes[i + 2] != b']' {
            let lo = c;
            let hi = bytes[i + 2];
            if lo <= hi {
                for b in lo..=hi {
                    fbs_set(&mut set, b);
                }
            }
            i += 3;
            continue;
        }
        fbs_set(&mut set, c);
        i += 1;
    }
    None // Unterminated class — give up.
}

/// How a single pattern matcher operates.
#[derive(Clone)]
pub enum MatcherKind {
    LuaPattern {
        code: String,
    },
    Regex {
        compiled: Arc<Regex>,
        /// `\G(?:...)`-anchored variant of the same pattern. PCRE2 treats a
        /// leading `\G` as true anchoring to the match start offset, so an
        /// open-matcher probe at a position that does not match returns in
        /// O(pattern) instead of scanning forward to the next match and
        /// discarding it. That collapses the per-line tokenize cost from
        /// O(line_len^2) to O(line_len) on long/minified lines. `None` when
        /// the anchored form failed to compile; the caller then falls back
        /// to the non-anchored regex plus a start-offset equality check.
        anchored: Option<Arc<Regex>>,
    },
}

/// A single pattern matcher with its anchor state.
#[derive(Clone)]
pub struct MatcherDef {
    pub kind: MatcherKind,
    pub whole_line: bool,
    /// Conservatively-computed bitset of bytes the matcher's regex could
    /// match starting with. `None` means "any byte" — fall back to always
    /// trying the match. See `analyze_first_byte_set` for the contract.
    pub first_byte_set: Option<FirstByteSet>,
}

/// A pattern that matches either a single span or an open/close pair.
#[derive(Clone)]
pub enum PatternMatcher {
    Single(MatcherDef),
    Pair {
        open: MatcherDef,
        close: MatcherDef,
        escape_byte: Option<u8>,
    },
}

/// Reference to another syntax for sub-syntax patterns.
#[derive(Clone)]
pub enum SyntaxRef {
    /// Fully compiled nested syntax, shared via `Arc` so the same graph
    /// node referenced from multiple patterns deduplicates.
    Inline(Arc<CompiledSyntax>),
    /// External lookup by name. Reserved for cross-asset references;
    /// currently unresolved at tokenize time.
    Selector(String),
}

/// A compiled pattern definition ready for tokenization.
#[derive(Clone)]
pub struct PatternDef {
    pub matcher: PatternMatcher,
    pub token_types: Vec<String>,
    pub syntax_ref: Option<SyntaxRef>,
    pub disabled: bool,
}

impl PatternDef {
    /// First-byte set for the matcher's *open* side (the side the tokenizer
    /// looks for when scanning forward through a line). `None` means the
    /// analyzer couldn't prove a tight set and the caller must always run
    /// the regex.
    #[inline]
    pub fn open_first_byte_set(&self) -> Option<&FirstByteSet> {
        let m = match &self.matcher {
            PatternMatcher::Single(m) => m,
            PatternMatcher::Pair { open, .. } => open,
        };
        m.first_byte_set.as_ref()
    }
}

/// A fully compiled syntax: patterns + symbol map.
#[derive(Clone, Default)]
pub struct CompiledSyntax {
    pub patterns: Vec<PatternDef>,
    pub symbols: HashMap<String, String>,
}

/// Compile a PCRE2 regex pattern for the tokenizer.
pub fn compile_regex(pattern: &str) -> Result<Regex, RegexError> {
    let mut builder = RegexBuilder::new();
    builder.utf(true).ucp(true);
    // Enable PCRE2's JIT compiler when available. Matching a JIT-compiled
    // regex is ~2-10x faster than the bytecode interpreter for typical
    // syntax patterns, and `jit_if_available` silently falls back to the
    // interpreter on builds without JIT support — so this is a free win
    // on supported platforms and a no-op everywhere else.
    builder.jit_if_available(true);
    builder
        .build(pattern)
        .map_err(|e| RegexError::Compile(e.to_string()))
}

/// Convert a Lua pattern string to a PCRE2 regex.
/// Expand a `%x` Lua class inside a character class `[...]`.
fn lua_class_to_regex_in_bracket(ch: char) -> &'static str {
    match ch {
        'a' => "\\p{L}",
        'A' => "\\P{L}",
        'd' => "0-9",
        'D' => "^0-9",
        'w' => "\\w\\p{M}",
        'W' => "^\\w\\p{M}",
        's' => "\\s",
        'S' => "\\S",
        'l' => "\\p{Ll}",
        'L' => "\\P{Ll}",
        'u' => "\\p{Lu}",
        'U' => "\\P{Lu}",
        'p' => "!-/:-@\\[-`{-~",
        _ => "",
    }
}

/// Expand a `%x` Lua class outside brackets.
fn lua_class_to_regex(ch: char) -> &'static str {
    match ch {
        'a' => "\\p{L}",
        'A' => "\\P{L}",
        'd' => "\\d",
        'D' => "\\D",
        'w' => "[\\w\\p{M}]",
        'W' => "[^\\w\\p{M}]",
        's' => "\\s",
        'S' => "\\S",
        'l' => "\\p{Ll}",
        'L' => "\\P{Ll}",
        'u' => "\\p{Lu}",
        'U' => "\\P{Lu}",
        'p' => "[^\\w\\s]",
        'P' => "[\\w\\s]",
        'c' => "[\\x00-\\x1f]",
        _ => "",
    }
}

fn lua_pattern_to_regex(pat: &str) -> String {
    let mut out = String::new();
    let mut chars = pat.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '%' {
            if let Some(&next) = chars.peek() {
                chars.next();
                if next == 'f' {
                    // %f[set] -- Lua frontier pattern. Matches the empty
                    // string at a position where the previous character
                    // does NOT match `set` and the current character DOES.
                    // Map to PCRE2's `(?<![set])(?=[set])`. Emitting only
                    // the lookahead (the historical form) caused patterns
                    // like `%f[^>][^<]` to fire mid-content and swallow
                    // tag names as plain text in XML/HTML grammars.
                    if chars.peek() == Some(&'[') {
                        chars.next(); // consume '['
                        // Collect the bracket contents once so we can emit
                        // them in both the lookbehind and the lookahead.
                        let mut set = String::new();
                        while let Some(&c) = chars.peek() {
                            if c == ']' {
                                chars.next();
                                break;
                            }
                            if c == '%' {
                                chars.next();
                                if let Some(&nc) = chars.peek() {
                                    chars.next();
                                    let expanded = lua_class_to_regex_in_bracket(nc);
                                    if expanded.is_empty() {
                                        if "\\]^-".contains(nc) {
                                            set.push('\\');
                                        }
                                        set.push(nc);
                                    } else {
                                        set.push_str(expanded);
                                    }
                                }
                            } else {
                                chars.next();
                                set.push(c);
                            }
                        }
                        out.push_str("(?<![");
                        out.push_str(&set);
                        out.push_str("])(?=[");
                        out.push_str(&set);
                        out.push_str("])");
                    }
                } else if next == 'b' {
                    // %bxy balanced match -- approximate.
                    if let (Some(open), Some(_close)) = (chars.next(), chars.next()) {
                        if "\\.*+?^${}()|[]".contains(open) {
                            out.push('\\');
                        }
                        out.push(open);
                    }
                } else {
                    let expanded = lua_class_to_regex(next);
                    if expanded.is_empty() {
                        // Literal escape.
                        if "\\.*+?^${}()|[]".contains(next) {
                            out.push('\\');
                        }
                        out.push(next);
                    } else {
                        out.push_str(expanded);
                    }
                }
            }
        } else if ch == '[' {
            // Character class -- need to handle %x inside brackets.
            out.push('[');
            while let Some(&c) = chars.peek() {
                if c == ']' {
                    chars.next();
                    out.push(']');
                    break;
                }
                if c == '%' {
                    chars.next();
                    if let Some(&nc) = chars.peek() {
                        chars.next();
                        let expanded = lua_class_to_regex_in_bracket(nc);
                        if expanded.is_empty() {
                            if "\\]^-".contains(nc) {
                                out.push('\\');
                            }
                            out.push(nc);
                        } else {
                            out.push_str(expanded);
                        }
                    }
                } else {
                    chars.next();
                    out.push(c);
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Build a `MatcherDef` from a pattern or regex string.
pub fn make_matcher(kind_name: &str, code: String) -> Result<MatcherDef, RegexError> {
    let (code, whole_line) = split_anchor(code);
    // Convert Lua patterns to PCRE2 regex.
    let regex_code = if kind_name == "regex" {
        code
    } else {
        lua_pattern_to_regex(&code)
    };
    // Compute the conservative first-byte set BEFORE moving `regex_code`
    // into the matcher. If the regex itself fails to compile we fall back
    // to the LuaPattern variant, which the runtime never matches against
    // anyway, so the set is irrelevant there.
    let first_byte_set = analyze_first_byte_set(&regex_code);
    let kind = match compile_regex(&regex_code) {
        Ok(compiled) => {
            // Wrap in a non-capturing group so the `\G` anchor applies to the
            // whole pattern (not just the first alternative of a top-level
            // `a|b`). `(?:...)` and `\G` are both group-neutral, so capture
            // numbering matches the non-anchored variant exactly.
            let anchored = compile_regex(&format!("\\G(?:{regex_code})"))
                .ok()
                .map(Arc::new);
            MatcherKind::Regex {
                compiled: Arc::new(compiled),
                anchored,
            }
        }
        Err(_) => {
            // Fall back to storing as LuaPattern if regex compilation fails.
            MatcherKind::LuaPattern { code: regex_code }
        }
    };
    Ok(MatcherDef {
        kind,
        whole_line,
        first_byte_set,
    })
}

/// Run a regex matcher at a character offset and return 1-based character positions.
pub fn regex_find(
    matcher: &MatcherDef,
    text: &str,
    next: usize,
    anchored: bool,
) -> Result<Vec<usize>, RegexError> {
    let MatcherKind::Regex {
        compiled,
        anchored: anchored_variant,
    } = &matcher.kind
    else {
        return Ok(Vec::new());
    };

    let start_byte = ucharpos(text, next)
        .unwrap_or(text.len() + 1)
        .saturating_sub(1);
    // Use the `\G`-anchored regex when an anchored match is wanted: PCRE2 then
    // tries only at `start_byte` and returns immediately on a non-match instead
    // of scanning the rest of the line and having us discard it below.
    let use_anchored = anchored && anchored_variant.is_some();
    let re: &Regex = if use_anchored {
        anchored_variant.as_ref().unwrap()
    } else {
        compiled
    };
    let mut locs = re.capture_locations();
    match re.captures_read_at(&mut locs, text.as_bytes(), start_byte) {
        Ok(Some(_)) => {
            let Some((s, e)) = locs.get(0) else {
                return Ok(Vec::new());
            };
            // The anchored regex guarantees `s == start_byte`; only the
            // non-anchored fallback needs the explicit start-offset discard.
            if anchored && !use_anchored && s != start_byte {
                return Ok(Vec::new());
            }

            // group0 (start + end) plus at most one position per remaining
            // capture group; pre-size so the per-match push never reallocates
            // on this hot per-pattern-per-column path.
            let captures_len = compiled.captures_len();
            let mut res = Vec::with_capacity(2 + captures_len);
            res.push(s + 1);
            res.push(e);
            for i in 1..=captures_len {
                if let Some((cs, ce)) = locs.get(i) {
                    if cs == ce {
                        res.push(cs + 1);
                    }
                }
            }

            // `res[0]` is a 1-based byte start; the char column is the number of
            // chars before that byte, matching the capture conversion below, so
            // a match starting on a multibyte char lands on a char boundary.
            let char_pos_1 = if res[0] > next {
                prefix_ulen(text, res[0].saturating_sub(1)) + 1
            } else {
                next
            };
            let char_pos_2 = prefix_ulen(text, res[1]);
            res[0] = char_pos_1;
            res[1] = char_pos_2;
            for item in res.iter_mut().skip(2) {
                *item = prefix_ulen(text, item.saturating_sub(1)) + 1;
            }
            Ok(res)
        }
        Ok(None) => Ok(Vec::new()),
        Err(err) => Err(RegexError::Match(err.to_string())),
    }
}

/// Tokenize a single line using a compiled syntax, returning a flat list of tokens.
pub fn tokenize_line(syntax: &CompiledSyntax, line: &str) -> Vec<Token> {
    tokenize_line_with_state(syntax, line, &[]).0
}

/// Run a pattern's open or close matcher (with optional escape handling).
/// `at_start` anchors the match to the position; `close=true` selects the
/// close side of a Pair (or the only matcher for Singles).
pub fn find_text(
    line: &str,
    pattern: &PatternDef,
    offset: usize,
    at_start: bool,
    close: bool,
) -> Result<Vec<usize>, RegexError> {
    if pattern.disabled {
        return Ok(Vec::new());
    }
    let (matcher, escape_byte) = match &pattern.matcher {
        PatternMatcher::Single(m) => (m, None),
        PatternMatcher::Pair {
            open,
            close: closer,
            escape_byte,
        } => {
            if close {
                (closer, *escape_byte)
            } else {
                (open, *escape_byte)
            }
        }
    };
    if matcher.whole_line && offset > 1 {
        return Ok(Vec::new());
    }
    let anchored = at_start || matcher.whole_line;
    // Skipping an escaped delimiter advances the search offset and retries; a
    // loop keeps stack depth constant so a line full of escaped delimiters
    // cannot overflow the stack.
    let mut offset = offset;
    loop {
        let res = regex_find(matcher, line, offset, anchored)?;
        if res.is_empty() {
            return Ok(Vec::new());
        }

        let Some(escape_byte) = escape_byte else {
            return Ok(res);
        };
        // Count preceding escape bytes; odd count means the delimiter is
        // escaped, so skip it and look further. Mirrors legacy `find_text`.
        let mut count = 0usize;
        let mut i = res[0].saturating_sub(1);
        while i >= 1 {
            let byte = line.as_bytes().get(i - 1).copied();
            if byte != Some(escape_byte) {
                break;
            }
            count += 1;
            if i == 1 {
                break;
            }
            i -= 1;
        }
        if count % 2 == 0 {
            return Ok(res);
        }
        if at_start || !close {
            return Ok(Vec::new());
        }
        let new_offset = res[0].saturating_add(1);
        if new_offset <= offset {
            return Ok(Vec::new());
        }
        offset = new_offset;
    }
}

/// State derived from the byte-stack `state`: which compiled syntax is
/// currently active, which pair we're waiting to close, and how deep we
/// are in the nesting stack.
struct SyntaxStateView<'a> {
    current_syntax: &'a CompiledSyntax,
    /// `(parent_syntax, parent_pattern_idx)` when the active syntax was
    /// entered via a sub-syntax pair on `parent_syntax`. The close of
    /// `parent_syntax.patterns[parent_pattern_idx]` pops back out.
    subsyntax_info: Option<(&'a CompiledSyntax, usize)>,
    /// 1-based pattern index in `current_syntax` that opened the
    /// current pair (when no sub-syntax descent happened). `0` means
    /// no open pair at this level.
    current_pattern_idx: usize,
    /// 1-based depth in the state byte stack.
    current_level: usize,
}

fn retrieve_syntax_state<'a>(base: &'a CompiledSyntax, state: &[u16]) -> SyntaxStateView<'a> {
    let mut current_syntax: &'a CompiledSyntax = base;
    let mut subsyntax_info: Option<(&'a CompiledSyntax, usize)> = None;
    let mut current_pattern_idx = state.first().copied().unwrap_or(0) as usize;
    let mut current_level = 1usize;

    if current_pattern_idx > 0
        && current_syntax
            .patterns
            .get(current_pattern_idx - 1)
            .is_some()
    {
        for (i, target) in state.iter().enumerate() {
            let target = *target as usize;
            if target == 0 {
                break;
            }
            let Some(pattern) = current_syntax.patterns.get(target - 1) else {
                break;
            };
            match &pattern.syntax_ref {
                Some(SyntaxRef::Inline(sub)) => {
                    subsyntax_info = Some((current_syntax, target - 1));
                    current_syntax = sub.as_ref();
                    current_pattern_idx = 0;
                    current_level = i + 2;
                }
                _ => {
                    current_pattern_idx = target;
                    break;
                }
            }
        }
    }

    SyntaxStateView {
        current_syntax,
        subsyntax_info,
        current_pattern_idx,
        current_level,
    }
}

/// Decode the externally threaded `Vec<u8>` end-state into the internal
/// per-level `u16` stack. Each level occupies two little-endian bytes, so a
/// 1-based pattern index survives grammars with more than 255 patterns. A
/// trailing odd byte (which the encoder never produces) is read as a low
/// byte with a zero high byte.
fn decode_state(bytes: &[u8]) -> Vec<u16> {
    bytes
        .chunks(2)
        .map(|pair| u16::from(pair[0]) | (u16::from(pair.get(1).copied().unwrap_or(0)) << 8))
        .collect()
}

/// Encode the internal per-level `u16` stack into the externally threaded
/// `Vec<u8>` representation: two little-endian bytes per level. This keeps
/// the cross-module `Vec<u8>` interface (`TokenCache::line_end_states`)
/// stable while giving 16-bit pattern indices internally.
fn encode_state(state: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(state.len() * 2);
    for &level in state {
        out.push((level & 0xFF) as u8);
        out.push((level >> 8) as u8);
    }
    out
}

/// Tokenize one line carrying multi-line state across calls. `in_state`
/// is a byte stack: each level holds a 1-based pattern index encoded as two
/// little-endian bytes, descending into sub-syntaxes as Pair patterns with
/// `syntax_ref` are opened. Empty (or a single zero level) means "no
/// carry-over". Callers thread the returned state into the next line so
/// block comments, multi-line strings, and other paired constructs span
/// line boundaries.
pub fn tokenize_line_with_state(
    syntax: &CompiledSyntax,
    line: &str,
    in_state: &[u8],
) -> (Vec<Token>, Vec<u8>) {
    // Build the per-line char↔byte index once so every `ucharpos`,
    // `prefix_ulen`, and `usub` call inside this function runs in O(1).
    // See the `LineIndex` comment above for the why.
    prime_utf8_line_index(line);
    let mut tokens = Vec::new();
    let line_len = char_len(line);

    let mut state: Vec<u16> = decode_state(in_state);
    if state.is_empty() {
        state.push(0);
    }

    if line_len == 0 {
        return (tokens, finalize_state(state));
    }

    let mut syn_state = retrieve_syntax_state(syntax, &state);
    let mut i = 1usize;

    while i <= line_len {
        if syn_state.current_pattern_idx > 0 {
            let pattern_idx = syn_state.current_pattern_idx - 1;
            let current_syntax = syn_state.current_syntax;
            if let Some(pattern) = current_syntax.patterns.get(pattern_idx) {
                let find_results = find_text(line, pattern, i, false, true).unwrap_or_default();
                let s = find_results.first().copied();
                let e = find_results.get(1).copied();
                let token_type = pattern
                    .token_types
                    .first()
                    .map(String::as_str)
                    .unwrap_or("normal");

                let mut cont = true;
                if let Some((parent, sub_idx)) = syn_state.subsyntax_info {
                    if let Some(sub_pattern) = parent.patterns.get(sub_idx) {
                        let sub_find =
                            find_text(line, sub_pattern, i, false, true).unwrap_or_default();
                        if let Some(ss) = sub_find.first().copied() {
                            if s.is_none() || ss < s.unwrap_or(usize::MAX) {
                                if ss > i {
                                    let text_part = usub(line, i, ss - 1);
                                    push_token(&mut tokens, token_type, text_part);
                                }
                                i = ss;
                                cont = false;
                            }
                        }
                    }
                }

                if cont {
                    if let (Some(s), Some(e)) = (s, e) {
                        if s > i {
                            let text_part = usub(line, i, s - 1);
                            push_token(&mut tokens, token_type, text_part);
                        }
                        push_tokens(
                            &mut tokens,
                            &current_syntax.symbols,
                            &pattern.token_types,
                            line,
                            find_results,
                        );
                        let level_idx = syn_state.current_level.saturating_sub(1);
                        if level_idx < state.len() {
                            state[level_idx] = 0;
                        }
                        syn_state.current_pattern_idx = 0;
                        i = e + 1;
                    } else {
                        let text_part = usub(line, i, line_len);
                        push_token(&mut tokens, token_type, text_part);
                        return (tokens, finalize_state(state));
                    }
                }
            }
        }

        // Pop sub-syntaxes whose close matches at the current position.
        // Each pop emits the close token via the parent's pair pattern.
        while let Some((parent, sub_idx)) = syn_state.subsyntax_info {
            let Some(sub_pattern) = parent.patterns.get(sub_idx) else {
                break;
            };
            let find_results = find_text(line, sub_pattern, i, true, true).unwrap_or_default();
            let s = find_results.first().copied();
            let e = find_results.get(1).copied();
            if let (Some(_), Some(e)) = (s, e) {
                push_tokens(
                    &mut tokens,
                    &syn_state.current_syntax.symbols,
                    &sub_pattern.token_types,
                    line,
                    find_results,
                );
                syn_state.current_level = syn_state.current_level.saturating_sub(1);
                state.truncate(syn_state.current_level);
                if state.is_empty() {
                    state.push(0);
                }
                let level_idx = syn_state.current_level.saturating_sub(1);
                if level_idx < state.len() {
                    state[level_idx] = 0;
                }
                syn_state = retrieve_syntax_state(syntax, &state);
                i = e + 1;
                if i > line_len {
                    return (tokens, finalize_state(state));
                }
            } else {
                break;
            }
        }

        if i > line_len {
            break;
        }

        let current_syntax = syn_state.current_syntax;
        let mut matched = false;
        // Look up the raw byte at char position `i` once. Used by the
        // first-byte set fast-skip below — when a pattern's open matcher
        // declares its possible first-byte set, we can avoid running the
        // anchored regex match entirely whenever this byte isn't in it.
        // On a 500-char markdown line with 90 patterns, where most chars
        // are plain text that doesn't start any pattern, this collapses
        // ~45k regex calls per line to a few thousand.
        let current_byte: Option<u8> =
            ucharpos(line, i).and_then(|bp| line.as_bytes().get(bp - 1).copied());
        for (n, pattern) in current_syntax.patterns.iter().enumerate() {
            if let (Some(b), Some(set)) = (current_byte, pattern.open_first_byte_set()) {
                if !fbs_contains(set, b) {
                    continue;
                }
            }
            let find_results = find_text(line, pattern, i, true, false).unwrap_or_default();
            if find_results.len() < 2 {
                continue;
            }
            if find_results[0] > find_results[1] {
                continue;
            }

            let next_i = find_results[1] + 1;
            push_tokens(
                &mut tokens,
                &current_syntax.symbols,
                &pattern.token_types,
                line,
                find_results,
            );

            if matches!(pattern.matcher, PatternMatcher::Pair { .. }) {
                let level_idx = syn_state.current_level.saturating_sub(1);
                if level_idx >= state.len() {
                    state.push((n + 1) as u16);
                } else {
                    state[level_idx] = (n + 1) as u16;
                }
                if let Some(SyntaxRef::Inline(sub)) = &pattern.syntax_ref {
                    syn_state.current_level += 1;
                    syn_state.subsyntax_info = Some((current_syntax, n));
                    syn_state.current_syntax = sub.as_ref();
                    syn_state.current_pattern_idx = 0;
                } else {
                    syn_state.current_pattern_idx = n + 1;
                }
            }
            i = next_i;
            matched = true;
            break;
        }

        if !matched {
            let text_part = usub(line, i, i);
            push_token(&mut tokens, "normal", text_part);
            i += 1;
        }
    }

    (tokens, finalize_state(state))
}

/// Strip trailing zero levels (no carry-over) and encode the surviving
/// `u16` stack into the externally threaded `Vec<u8>` (two little-endian
/// bytes per level). Returns an empty vec when the entire stack is zero;
/// callers treat an empty state as "fresh".
fn finalize_state(mut state: Vec<u16>) -> Vec<u8> {
    while state.last() == Some(&0) {
        state.pop();
    }
    encode_state(&state)
}

/// Compile a `SyntaxDefinition` (from `native::syntax`) into a `CompiledSyntax`.
pub fn compile_from_definition(def: &SyntaxDefinition) -> Result<CompiledSyntax, RegexError> {
    let mut patterns = Vec::new();

    for rule in &def.patterns {
        let kind_name;
        let spec;
        if let Some(p) = &rule.pattern {
            kind_name = "pattern";
            spec = p;
        } else if let Some(r) = &rule.regex {
            kind_name = "regex";
            spec = r;
        } else {
            continue;
        };

        let matcher = match spec {
            PatternSpec::Single(code) => {
                PatternMatcher::Single(make_matcher(kind_name, code.clone())?)
            }
            PatternSpec::Pair {
                open,
                close,
                escape,
            } => {
                let open_def = make_matcher(kind_name, open.clone())?;
                let close_def = make_matcher(kind_name, close.clone())?;
                let escape_byte = escape.as_deref().and_then(first_byte);
                PatternMatcher::Pair {
                    open: open_def,
                    close: close_def,
                    escape_byte,
                }
            }
        };

        let token_types = match &rule.token_type {
            TokenType::Single(s) => vec![s.clone()],
            TokenType::Multi(v) => v.clone(),
        };

        let syntax_ref = match &rule.syntax {
            Some(SubSyntaxSpec::Selector(name)) => Some(SyntaxRef::Selector(name.clone())),
            Some(SubSyntaxSpec::Inline(sub_def)) => {
                let sub_compiled = compile_from_definition(sub_def)?;
                Some(SyntaxRef::Inline(Arc::new(sub_compiled)))
            }
            None => None,
        };

        patterns.push(PatternDef {
            matcher,
            token_types,
            syntax_ref,
            disabled: false,
        });
    }

    Ok(CompiledSyntax {
        patterns,
        symbols: def.symbols.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn data_dir() -> String {
        for candidate in ["data", "../data"] {
            if Path::new(candidate).join("assets/syntax").is_dir() {
                return candidate.to_string();
            }
        }
        panic!("cannot locate data/ directory");
    }

    fn compile_single_pattern(pattern: &str, token_type: &str) -> CompiledSyntax {
        let def = SyntaxDefinition {
            name: "Test".into(),
            patterns: vec![crate::editor::syntax::PatternRule {
                pattern: Some(PatternSpec::Single(pattern.into())),
                regex: None,
                token_type: TokenType::Single(token_type.into()),
                syntax: None,
            }],
            ..Default::default()
        };
        compile_from_definition(&def).unwrap()
    }

    fn markdown_syntax() -> CompiledSyntax {
        let defs = crate::editor::syntax::load_syntax_assets(&data_dir());
        let def = defs
            .into_iter()
            .find(|def| def.name == "Markdown")
            .expect("should load Markdown syntax asset");
        compile_from_definition(&def).expect("should compile Markdown syntax")
    }

    #[test]
    fn char_len_ascii() {
        assert_eq!(char_len("hello"), 5);
    }

    #[test]
    fn char_len_multibyte() {
        assert_eq!(char_len("\u{00E9}\u{00E8}"), 2);
    }

    #[test]
    fn usub_basic() {
        assert_eq!(usub("hello", 2, 4), "ell");
    }

    #[test]
    fn usub_full() {
        assert_eq!(usub("hello", 1, 5), "hello");
    }

    #[test]
    fn usub_empty() {
        assert_eq!(usub("hello", 3, 2), "");
    }

    #[test]
    fn prefix_ulen_basic() {
        assert_eq!(prefix_ulen("hello", 3), 3);
    }

    #[test]
    fn prefix_ulen_multibyte() {
        let s = "\u{00E9}bc";
        assert_eq!(prefix_ulen(s, 2), 1); // 2 bytes = 1 char for e-acute
    }

    #[test]
    fn ucharpos_basic() {
        assert_eq!(ucharpos("hello", 3), Some(3));
        assert_eq!(ucharpos("hello", 0), Some(1));
    }

    #[test]
    fn split_anchor_with_caret() {
        let (code, anchored) = split_anchor("^foo".to_string());
        assert_eq!(code, "foo");
        assert!(anchored);
    }

    #[test]
    fn split_anchor_without() {
        let (code, anchored) = split_anchor("foo".to_string());
        assert_eq!(code, "foo");
        assert!(!anchored);
    }

    #[test]
    fn push_token_merges_same_type() {
        let mut tokens = Vec::new();
        push_token(&mut tokens, "normal", "hello");
        push_token(&mut tokens, "normal", " world");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].text, "hello world");
    }

    #[test]
    fn push_token_merges_whitespace_into_next() {
        let mut tokens = Vec::new();
        push_token(&mut tokens, "normal", "  ");
        push_token(&mut tokens, "keyword", "if");
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].text, "  if");
        assert_eq!(tokens[0].token_type.as_ref(), "keyword");
    }

    #[test]
    fn push_token_separates_different_types() {
        let mut tokens = Vec::new();
        push_token(&mut tokens, "keyword", "if");
        push_token(&mut tokens, "normal", " x");
        assert_eq!(tokens.len(), 2);
    }

    #[test]
    fn compile_from_definition_basic() {
        let def = SyntaxDefinition {
            name: "Test".into(),
            patterns: vec![crate::editor::syntax::PatternRule {
                pattern: Some(PatternSpec::Single("%w+".into())),
                regex: None,
                token_type: TokenType::Single("symbol".into()),
                syntax: None,
            }],
            symbols: HashMap::from([("if".into(), "keyword".into())]),
            ..Default::default()
        };
        let compiled = compile_from_definition(&def).unwrap();
        assert_eq!(compiled.patterns.len(), 1);
        assert_eq!(compiled.symbols.get("if"), Some(&"keyword".to_string()));
    }

    #[test]
    fn compile_from_definition_regex() {
        let def = SyntaxDefinition {
            name: "RegexTest".into(),
            patterns: vec![crate::editor::syntax::PatternRule {
                pattern: None,
                regex: Some(PatternSpec::Single(r"\d+".into())),
                token_type: TokenType::Single("number".into()),
                syntax: None,
            }],
            symbols: HashMap::new(),
            ..Default::default()
        };
        let compiled = compile_from_definition(&def).unwrap();
        assert_eq!(compiled.patterns.len(), 1);
        assert!(matches!(
            compiled.patterns[0].matcher,
            PatternMatcher::Single(MatcherDef {
                kind: MatcherKind::Regex { .. },
                ..
            })
        ));
    }

    #[test]
    fn regex_find_basic() {
        let matcher = make_matcher("regex", r"\d+".to_string()).unwrap();
        let results = regex_find(&matcher, "abc 123 def", 1, false).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0], 5); // '1' is at char position 5
        assert_eq!(results[1], 7); // '3' is at char position 7
    }

    #[test]
    fn state_codec_round_trips_index_above_u8() {
        // A grammar with more than 255 patterns yields level indices that
        // overflow a single byte; the two-byte little-endian threading must
        // survive the external `Vec<u8>` interface without truncation.
        let levels: Vec<u16> = vec![1, 255, 256, 300, 1000, 65535, 0, 42];
        let encoded = encode_state(&levels);
        assert_eq!(encoded.len(), levels.len() * 2);
        assert_eq!(decode_state(&encoded), levels);

        // A bare value above 255 must round-trip as itself, not its low byte.
        assert_eq!(decode_state(&encode_state(&[300])), vec![300u16]);
    }

    #[test]
    fn lua_pattern_to_regex_uses_unicode_properties() {
        assert_eq!(lua_pattern_to_regex("%a"), r"\p{L}");
        assert_eq!(lua_pattern_to_regex("[%a_]"), r"[\p{L}_]");
        assert_eq!(lua_pattern_to_regex("[%w_]"), r"[\w\p{M}_]");
        assert_eq!(lua_pattern_to_regex("%u"), r"\p{Lu}");
        assert_eq!(lua_pattern_to_regex("%l"), r"\p{Ll}");
    }

    #[test]
    fn tokenize_line_matches_unicode_words() {
        let syntax = compile_single_pattern("[%a_][%w_]*", "symbol");
        for line in [
            "\u{00E1}rv\u{00ED}zt\u{0171}r\u{0151}",
            "f\u{00FC}ggv\u{00E9}ny_1",
            "a\u{0301}r",
        ] {
            let tokens = tokenize_line(&syntax, line);
            assert_eq!(
                tokens,
                vec![Token {
                    token_type: "symbol".into(),
                    text: line.into(),
                }],
                "expected full-line symbol token for {line:?}"
            );
        }
    }

    #[test]
    fn tokenize_line_matches_unicode_case_classes() {
        let syntax = compile_single_pattern("<[%u%l][%w_%.:-]*>", "tag");
        let line = "<\u{00C1}rWidget>";
        let tokens = tokenize_line(&syntax, line);
        assert_eq!(
            tokens,
            vec![Token {
                token_type: "tag".into(),
                text: line.into(),
            }]
        );
    }

    /// The text covered by any emphasis token (bold / italic / bold-italic).
    fn emphasized_text(syntax: &CompiledSyntax, line: &str) -> (String, Vec<u8>) {
        let (tokens, state) = tokenize_line_with_state(syntax, line, &[]);
        let joined: String = tokens.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(joined, line, "tokens must round-trip the line {line:?}");
        let emph: String = tokens
            .iter()
            .filter(|t| {
                matches!(
                    t.token_type.as_ref(),
                    "markdown_bold" | "markdown_italic" | "markdown_bold_italic"
                )
            })
            .map(|t| t.text.as_str())
            .collect();
        (emph, state)
    }

    #[test]
    fn markdown_emphasis_requires_no_inner_whitespace() {
        let syntax = markdown_syntax();
        // Valid CommonMark emphasis: delimiters hug the content.
        for (line, expected) in [
            ("**x**", "**x**"),
            ("*x*", "*x*"),
            ("***x***", "***x***"),
            ("__x__", "__x__"),
            ("_x_", "_x_"),
            ("a **bold** b", " **bold**"),
            ("a *it* b", " *it*"),
        ] {
            let (emph, state) = emphasized_text(&syntax, line);
            assert_eq!(emph, expected, "{line:?} should emphasize {expected:?}");
            assert!(
                state.is_empty(),
                "{line:?} must not carry state, got {state:?}"
            );
        }

        // Whitespace between a delimiter and its content invalidates the
        // emphasis (CommonMark flanking rule). Nothing should be emphasized
        // and, crucially, no state may leak to the next line.
        for line in [
            "** x **",
            "** x**",
            "**x **",
            "*** x ***",
            "a ** notbold ** b",
            "_ x _",
            "__ x __",
            "x = 2 * y + 3 * z",
            "use a * b and c * d",
        ] {
            let (emph, state) = emphasized_text(&syntax, line);
            assert!(
                emph.is_empty(),
                "{line:?} should not emphasize, got {emph:?}"
            );
            assert!(
                state.is_empty(),
                "{line:?} must not carry state, got {state:?}"
            );
        }
    }

    #[test]
    fn markdown_emphasis_does_not_bleed_to_following_lines() {
        // Regression: an emphasis delimiter with no valid partner used to
        // open a pair whose close never matched, carrying open state to the
        // next line and rendering the rest of the document emphasized.
        let syntax = markdown_syntax();
        for opener in ["** x **", "*x*", "a single * here", "trailing star *"] {
            let (_t1, s1) = tokenize_line_with_state(&syntax, opener, &[]);
            assert!(
                s1.is_empty(),
                "{opener:?} must not leave open emphasis state, got {s1:?}"
            );
            let (tokens, s2) = tokenize_line_with_state(&syntax, "plain next line", &s1);
            assert!(s2.is_empty(), "follow-up line should stay stateless");
            assert!(
                tokens
                    .iter()
                    .all(|t| t.token_type.as_ref() != "markdown_italic"
                        && t.token_type.as_ref() != "markdown_bold"
                        && t.token_type.as_ref() != "markdown_bold_italic"),
                "follow-up line must not inherit emphasis, got {tokens:?}"
            );
        }
    }

    #[test]
    fn markdown_asset_highlights_unicode_italic_text() {
        let syntax = markdown_syntax();
        let line = "_\u{00E1}rv\u{00ED}zt\u{0171}r\u{0151}_";
        let tokens = tokenize_line(&syntax, line);
        assert_eq!(
            tokens
                .iter()
                .map(|token| token.text.as_str())
                .collect::<String>(),
            line
        );
        assert!(
            tokens
                .iter()
                .any(|token| token.token_type.as_ref() != "normal"),
            "expected Markdown syntax to apply a non-normal token to {line:?}"
        );
    }

    #[test]
    fn markdown_emphasis_handles_adversarial_delimiter_runs() {
        // The emphasis regexes use non-greedy bodies with mutually exclusive
        // alternatives, so adversarial delimiter runs must still terminate and
        // round-trip every byte rather than backtracking pathologically.
        let syntax = markdown_syntax();
        for line in [
            "*".repeat(200),
            "_".repeat(200),
            format!("**{}", "a".repeat(500)),
            format!("{}**", "a *".repeat(200)),
            "*_*_*_*_*_*_".repeat(40),
        ] {
            let (tokens, _state) = tokenize_line_with_state(&syntax, &line, &[]);
            let joined: String = tokens.iter().map(|t| t.text.as_str()).collect();
            assert_eq!(joined, line, "adversarial line must round-trip");
        }
    }

    fn python_syntax() -> CompiledSyntax {
        let defs = crate::editor::syntax::load_syntax_assets(&data_dir());
        let def = defs
            .into_iter()
            .find(|def| def.name == "Python")
            .expect("should load Python syntax asset");
        compile_from_definition(&def).expect("should compile Python syntax")
    }

    #[test]
    fn python_for_loop_tokenizes_keywords_and_close() {
        // Regression: the simplified tokenizer used to collapse pair-syntax
        // patterns (for/if/etc.) into one flat token, eating the entire line
        // including the `:` and beyond. With sub-syntax descent, `for` is
        // keyword, `in` is keyword (via symbol map), and the close `:`
        // arrives after the bracketed `[1:]`, not the first colon inside it.
        let syntax = python_syntax();
        let line = "for f in sys.argv[1:]: cat(f)";
        let tokens = tokenize_line(&syntax, line);
        let joined: String = tokens.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(joined, line, "every byte must round-trip");
        let has_for_keyword = tokens
            .iter()
            .any(|t| t.text == "for" && t.token_type.as_ref() == "keyword");
        assert!(has_for_keyword, "`for` should be keyword, got {tokens:?}");
        let has_in_keyword = tokens
            .iter()
            .any(|t| t.text == "in" && t.token_type.as_ref() == "keyword");
        assert!(has_in_keyword, "`in` should be keyword, got {tokens:?}");
        let has_cat_function = tokens
            .iter()
            .any(|t| t.text == "cat" && t.token_type.as_ref() == "function");
        assert!(has_cat_function, "`cat` should be function, got {tokens:?}");
    }

    #[test]
    fn markdown_asset_highlights_unicode_heading_text() {
        let syntax = markdown_syntax();
        let line = "# \u{00C1}rv\u{00ED}zt\u{0171}r\u{0151} t\u{00FC}k\u{00F6}rf\u{00FA}r\u{00F3}g\u{00E9}p {#arvizturo}";
        let tokens = tokenize_line(&syntax, line);
        let joined: String = tokens.iter().map(|t| t.text.as_str()).collect();
        assert_eq!(
            joined, line,
            "every byte should round-trip through the token stream"
        );
        let first = tokens.first().expect("at least one token");
        assert_eq!(first.token_type.as_ref(), "keyword");
        assert!(
            first
                .text
                .starts_with("# \u{00C1}rv\u{00ED}zt\u{0171}r\u{0151}"),
            "heading text should be one keyword run, got {first:?}"
        );
    }

    /// Brute-force correctness check for `analyze_first_byte_set`: for every
    /// pattern in every bundled syntax, build a small input that begins
    /// with each ASCII byte the analyzer excluded from the pattern's
    /// first-byte set, run the anchored PCRE2 match, and confirm it does
    /// not match. A regex that *can* match anchored at byte `b` whose set
    /// excludes `b` would be a false negative — the runtime skip would
    /// drop a valid match, producing wrong tokens. Runs in milliseconds
    /// because most rejected bytes get rejected by PCRE2 immediately.
    #[test]
    fn first_byte_sets_have_no_false_negatives() {
        let defs = crate::editor::syntax::load_syntax_assets(&data_dir());
        let mut checked = 0usize;
        for def in &defs {
            let Ok(compiled) = compile_from_definition(def) else {
                continue;
            };
            for pat in &compiled.patterns {
                check_matcher(&pat.matcher, &def.name, &mut checked);
            }
        }
        assert!(checked > 0, "no patterns scanned — syntax data missing?");
        eprintln!("first_byte_sets_have_no_false_negatives: checked {checked} matchers");
    }

    fn check_matcher(m: &PatternMatcher, name: &str, checked: &mut usize) {
        match m {
            PatternMatcher::Single(md) => check_one(md, name, "single", checked),
            PatternMatcher::Pair { open, close, .. } => {
                check_one(open, name, "pair.open", checked);
                check_one(close, name, "pair.close", checked);
            }
        }
    }

    fn check_one(md: &MatcherDef, syntax_name: &str, slot: &str, checked: &mut usize) {
        let Some(set) = &md.first_byte_set else {
            return;
        };
        *checked += 1;
        let MatcherKind::Regex { compiled, .. } = &md.kind else {
            return;
        };
        for b in 1u8..=126 {
            if fbs_contains(set, b) {
                continue;
            }
            // Build a probe that starts with `b` and supplies likely
            // padding (punctuation + alnums) so most regex shapes can
            // satisfy a min-length / capture-group requirement.
            let mut buf = Vec::with_capacity(33);
            buf.push(b);
            buf.extend_from_slice(b"abcDEF012_-.()[]+/*?<>={}|");
            let mut locs = compiled.capture_locations();
            if let Ok(Some(_)) = compiled.captures_read_at(&mut locs, &buf, 0) {
                if let Some((s, _e)) = locs.get(0) {
                    assert!(
                        s != 0,
                        "FALSE NEGATIVE: syntax={syntax_name} {slot} regex matched anchored at byte 0x{b:02x} but analyzer excluded it. buf={:?}",
                        std::str::from_utf8(&buf).unwrap_or("<invalid utf8>"),
                    );
                }
            }
        }
    }
}
