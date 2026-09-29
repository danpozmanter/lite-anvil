//! Incremental relexing with per-line lexer states.
//!
//! The tokenizer is a state machine whose state threads across lines: block
//! comments, triple-quoted strings, and embedded sub-syntaxes all live in the
//! state one line hands to the next. Re-tokenizing a line in isolation is
//! therefore wrong, and re-tokenizing everything after an edit is what made
//! typing in large files flicker (see changelog 2.16.8).
//!
//! This module is the explicit form of the model the editor's token cache
//! implements lazily in its render walk (`doc_view.rs`): save the lexer state
//! after every line; on an edit, relex starting at the first changed line and
//! stop early once a line's end state equals the state previously saved for
//! that line — everything below then starts from the same state over
//! identical content, so its saved tokens remain exact. Relex work is bounded
//! by how far the edit can reach lexically, not by file length, and a change
//! that cannot reach past its line stops at that line.

use crate::editor::open_doc::CachedLine;
use crate::editor::tokenizer::{tokenize_line_with_state, CompiledSyntax, Token};

/// Tokens and the lexer state after one relexed line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelexedLine {
    pub tokens: Vec<Token>,
    pub end_state: Vec<u8>,
}

/// What one incremental relex did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RelexStats {
    /// 1-based number of the first line tokenized.
    pub first_line: usize,
    /// 1-based number of the last line tokenized (inclusive).
    pub last_line: usize,
    /// Lines actually tokenized.
    pub lines_tokenized: usize,
    /// True when the relex stopped because a line's new end state matched the
    /// state saved for that line, rather than reaching end-of-file.
    pub stopped_early: bool,
}

/// Tokenize every line from a fresh state, threading state across lines.
/// This is the baseline an incremental relex must match exactly.
pub fn full_relex(syntax: &CompiledSyntax, lines: &[&str]) -> Vec<RelexedLine> {
    let mut state: Vec<u8> = Vec::new();
    lines
        .iter()
        .map(|line| {
            let (tokens, end) = tokenize_line_with_state(syntax, line, &state);
            state = end;
            RelexedLine {
                tokens,
                end_state: state.clone(),
            }
        })
        .collect()
}

/// Relex `lines` (the current content) starting at `first_changed` (1-based).
///
/// `saved_end_states` are the end states previously produced for the content
/// now at each line, indexed by current line number minus one: entry `j`
/// holds the state produced for the content now at line `j + 1`, or `None`
/// where the edit inserted content and no prior state exists. Lines below
/// `first_changed` must be content-identical to what those saved states were
/// produced from — an insertion or deletion shifts the saved states to keep
/// that true.
///
/// Tokenizes from `first_changed`, threading state, and stops at the first
/// line whose new end state equals the state saved for that line: every line
/// below starts from the same state over identical content, so its saved
/// tokens are still exact. Returns the tokens and end state for each relexed
/// line plus [`RelexStats`] describing the span.
pub fn relex_span(
    syntax: &CompiledSyntax,
    lines: &[&str],
    saved_end_states: &[Option<Vec<u8>>],
    first_changed: usize,
) -> (Vec<RelexedLine>, RelexStats) {
    let mut stats = RelexStats {
        first_line: first_changed,
        ..Default::default()
    };
    if lines.is_empty() || first_changed > lines.len() {
        return (Vec::new(), stats);
    }
    let first_changed = first_changed.max(1);

    // The state entering the first changed line is the end state saved for
    // the unchanged line above it: lines 1..first_changed-1 are
    // content-identical to what those states were produced from, so the
    // saved state threads straight in. Empty for the first line (fresh
    // start) or when nothing was saved for the line above.
    let mut state: Vec<u8> = if first_changed > 1 {
        saved_end_states
            .get(first_changed - 2)
            .and_then(|s| s.as_ref())
            .cloned()
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut relexed = Vec::new();
    for (idx, line) in lines[first_changed - 1..].iter().enumerate() {
        let ln = first_changed + idx;
        let (tokens, end) = tokenize_line_with_state(syntax, line, &state);
        state = end;
        let saved = saved_end_states
            .get(ln - 1)
            .and_then(|s| s.as_ref())
            .cloned();
        let stops_here = saved.as_deref() == Some(state.as_slice());
        relexed.push(RelexedLine {
            tokens,
            end_state: state.clone(),
        });
        stats.last_line = ln;
        stats.lines_tokenized += 1;
        if stops_here && ln < lines.len() {
            stats.stopped_early = true;
            break;
        }
    }
    (relexed, stats)
}

/// The cache-entry validation predicate shared by the editor's render walk:
/// an entry is exact when its content hash matches the line and its saved
/// start state matches the state entering the line. Tokenization is
/// deterministic over `(content, start_state)`, so a validating entry is
/// correct regardless of edit history — this is what lets the walk stop
/// relexing at the first line the edit cannot reach.
pub(crate) fn entry_validates(
    entry: &CachedLine,
    content_hash: u64,
    incoming_state: &[u8],
) -> bool {
    entry.content_hash == content_hash && entry.start_state.as_slice() == incoming_state
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::syntax::load_syntax_assets;
    use std::path::Path;

    fn data_dir() -> String {
        for candidate in ["data", "../data"] {
            if Path::new(candidate).join("assets/syntax").is_dir() {
                return candidate.to_string();
            }
        }
        panic!("cannot locate data/ directory");
    }

    fn compiled_syntax(name: &str, filename: &str) -> CompiledSyntax {
        let data_dir = format!("{}/../data", env!("CARGO_MANIFEST_DIR"));
        let index = crate::editor::syntax::load_syntax_index(&data_dir);
        crate::editor::tokenizer::compile_for_filename(filename, &index)
            .expect("grammar compiles")
            .unwrap_or_else(|| panic!("{name} grammar matches {filename}"))
    }

    fn python_syntax() -> CompiledSyntax {
        compiled_syntax("Python", "chart.py")
    }

    fn html_syntax() -> CompiledSyntax {
        compiled_syntax("HTML", "index.html")
    }

    fn markdown_syntax() -> CompiledSyntax {
        let defs = load_syntax_assets(&data_dir());
        let def = defs
            .into_iter()
            .find(|def| def.name == "Markdown")
            .expect("Markdown grammar");
        crate::editor::tokenizer::compile_from_definition(&def).expect("Markdown compiles")
    }

    fn python_lines() -> Vec<String> {
        vec![
            "import os".into(),
            "".into(),
            "def bench(runs):".into(),
            "    total = 0".into(),
            "    for r in runs".into(),
            "        total += r".into(),
            "    return f\"\"\"<table>".into(),
            "  {rows}".into(),
            "</table>\"\"\"".into(),
            "".into(),
            "def main() -> int:".into(),
            "    return 0".into(),
        ]
    }

    fn joined(tokens: &[Token]) -> String {
        tokens.iter().map(|t| t.text.as_str()).collect()
    }

    #[test]
    fn incremental_relex_matches_full_relex_after_edit() {
        let syntax = python_syntax();
        let lines = python_lines();
        let before = full_relex(&syntax, &lines.iter().map(String::as_str).collect::<Vec<_>>());

        // Edit the interior of the multi-line f-string; no line count change.
        let mut edited = lines.clone();
        edited[7] = "  {rows} sorted".into();
        let saved: Vec<Option<Vec<u8>>> =
            before.iter().map(|l| Some(l.end_state.clone())).collect();
        let (relexed, stats) =
            relex_span(&syntax, &edited.iter().map(String::as_str).collect::<Vec<_>>(), &saved, 8);

        // The relexed span must be token-for-token identical to a full relex
        // of the edited content.
        let after = full_relex(&syntax, &edited.iter().map(String::as_str).collect::<Vec<_>>());
        for (n, line) in relexed.iter().enumerate() {
            let ln = 8 + n;
            assert_eq!(line.tokens, after[ln - 1].tokens, "line {ln} tokens");
        }
        // States below the early stop are the saved ones, and they match a
        // full relex of the new content.
        for (n, line) in relexed.iter().enumerate() {
            assert_eq!(line.end_state, after[8 + n - 1].end_state, "line {n}");
        }

        // The edit leaves the f-string-open state unchanged, so the new end
        // state of the edited line equals the saved one: the relex stops on
        // that line — one line of work — long before EOF.
        assert!(stats.stopped_early, "expected an early stop, got {stats:?}");
        assert_eq!(stats.last_line, 8);
        assert_eq!(stats.lines_tokenized, 1);
    }

    #[test]
    fn incremental_relex_stops_at_first_unchanged_end_state() {
        let syntax = python_syntax();
        let lines = python_lines();
        let before = full_relex(&syntax, &lines.iter().map(String::as_str).collect::<Vec<_>>());

        // Editing a plain code line leaves its end state empty, exactly the
        // saved state: the relex must stop on that line and tokenize nothing
        // below it.
        let mut edited = lines.clone();
        edited[3] = "    total = 1".into();
        let saved: Vec<Option<Vec<u8>>> =
            before.iter().map(|l| Some(l.end_state.clone())).collect();
        let (relexed, stats) =
            relex_span(&syntax, &edited.iter().map(String::as_str).collect::<Vec<_>>(), &saved, 4);
        assert_eq!(stats.lines_tokenized, 1, "one line relexed, got {stats:?}");
        assert_eq!(stats.last_line, 4);
        assert!(stats.stopped_early);
        assert_eq!(relexed[0].end_state, Vec::<u8>::new());

        // An edit that opens an unterminated construct changes every end
        // state below it, so the relex runs to end-of-file without stopping.
        let mut unclosed = lines.clone();
        unclosed[3] = "    s = \"unterminated".into();
        let (_, stats) =
            relex_span(&syntax, &unclosed.iter().map(String::as_str).collect::<Vec<_>>(), &saved, 4);
        assert!(!stats.stopped_early, "expected relex to EOF, got {stats:?}");
        assert_eq!(stats.last_line, unclosed.len());
    }

    #[test]
    fn incremental_relex_survives_line_insertion_and_deletion() {
        let syntax = python_syntax();
        let lines = python_lines();
        let before = full_relex(&syntax, &lines.iter().map(String::as_str).collect::<Vec<_>>());
        let old_states: Vec<Option<Vec<u8>>> =
            before.iter().map(|l| Some(l.end_state.clone())).collect();

        // Insert a comment line after line 4: saved states for the shifted
        // content move down one, the inserted line has no saved state.
        let mut inserted = lines.clone();
        inserted.insert(4, "    # tail".into());
        let mut saved: Vec<Option<Vec<u8>>> = old_states[..4].to_vec();
        saved.push(None);
        saved.extend_from_slice(&old_states[4..]);
        let (relexed, stats) =
            relex_span(&syntax, &inserted.iter().map(String::as_str).collect::<Vec<_>>(), &saved, 5);

        let after = full_relex(&syntax, &inserted.iter().map(String::as_str).collect::<Vec<_>>());
        for (n, line) in relexed.iter().enumerate() {
            assert_eq!(line.tokens, after[4 + n].tokens, "insertion: line {}", 5 + n);
            assert_eq!(line.end_state, after[4 + n].end_state);
        }
        // The comment leaves the state empty, the next line resyncs, and the
        // relex stops there.
        assert!(stats.stopped_early, "insertion: {stats:?}");
        assert!(stats.last_line < inserted.len());

        // Delete line 5 (`    for r in runs`): saved states shift up one.
        let mut deleted = lines.clone();
        deleted.remove(4);
        let saved: Vec<Option<Vec<u8>>> = old_states[..4]
            .iter()
            .chain(old_states[5..].iter())
            .cloned()
            .collect();
        let (relexed, stats) =
            relex_span(&syntax, &deleted.iter().map(String::as_str).collect::<Vec<_>>(), &saved, 5);
        let after = full_relex(&syntax, &deleted.iter().map(String::as_str).collect::<Vec<_>>());
        for (n, line) in relexed.iter().enumerate() {
            assert_eq!(line.tokens, after[4 + n].tokens, "deletion: line {}", 5 + n);
            assert_eq!(line.end_state, after[4 + n].end_state);
        }
        assert!(stats.stopped_early, "deletion: {stats:?}");
    }

    /// Regression for the 2.16.2 embedded-language highlighting bug: editing
    /// a line inside an HTML `<script>` block must relex it with the carried
    /// sub-syntax state, so the JavaScript inside stays highlighted. A relex
    /// that restarts from a fresh state at the changed line renders the
    /// edited line as plain host-grammar text.
    #[test]
    fn relex_preserves_embedded_javascript_after_edit() {
        let syntax = html_syntax();
        let lines: Vec<String> = [
            "<html>",
            "<body>",
            "<script>",
            "function f() {",
            "  return 1;",
            "var x = 2;",
            "</script>",
            "</body>",
            "</html>",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let before = full_relex(&syntax, &lines.iter().map(String::as_str).collect::<Vec<_>>());

        let mut edited = lines.clone();
        edited[5] = "var x = 42;".into();
        let saved: Vec<Option<Vec<u8>>> =
            before.iter().map(|l| Some(l.end_state.clone())).collect();
        let (relexed, stats) =
            relex_span(&syntax, &edited.iter().map(String::as_str).collect::<Vec<_>>(), &saved, 6);

        // `var` is still inside the <script> sub-syntax: keyword-highlighted.
        assert!(
            relexed[0]
                .tokens
                .iter()
                .any(|t| &*t.token_type == "keyword" && t.text.trim() == "var"),
            "edited line lost its JavaScript highlighting: {:?}",
            relexed[0].tokens
        );

        // Identical to a full relex of the edited content, and the edit
        // leaves the <script>-open state unchanged, so the relex stops on
        // the edited line itself.
        let after = full_relex(&syntax, &edited.iter().map(String::as_str).collect::<Vec<_>>());
        for (n, line) in relexed.iter().enumerate() {
            assert_eq!(line.tokens, after[5 + n].tokens, "line {}", 6 + n);
        }
        assert!(stats.stopped_early, "expected an early stop, got {stats:?}");
        assert_eq!(stats.last_line, 6);
        assert_eq!(stats.lines_tokenized, 1);
    }

    /// Regression for the 2.16.7 Python cross-line leak: the doc relexed
    /// after an edit around the multi-line f-string must leave the code
    /// below it (`def main`) code-typed, matching a full relex — not carry
    /// string state past the closing `"""`.
    #[test]
    fn relex_pins_python_string_state_below_a_multi_line_f_string() {
        let syntax = python_syntax();
        let lines = python_lines();
        let before = full_relex(&syntax, &lines.iter().map(String::as_str).collect::<Vec<_>>());

        // Edit the f-string opener line itself.
        let mut edited = lines.clone();
        edited[6] = "    return f\"\"\"<table class=t>".into();
        let saved: Vec<Option<Vec<u8>>> =
            before.iter().map(|l| Some(l.end_state.clone())).collect();
        let (relexed, stats) =
            relex_span(&syntax, &edited.iter().map(String::as_str).collect::<Vec<_>>(), &saved, 7);

        let after = full_relex(&syntax, &edited.iter().map(String::as_str).collect::<Vec<_>>());
        for (n, line) in relexed.iter().enumerate() {
            assert_eq!(line.tokens, after[6 + n].tokens, "line {}", 7 + n);
        }

        // The edit leaves the f-string-open state identical, so the relex
        // stops on the edited line itself; the interior and closing lines
        // keep their exact saved tokens, and `def main` below is untouched.
        assert!(stats.stopped_early, "expected an early stop, got {stats:?}");
        assert_eq!(stats.last_line, 7);

        // And those saved tokens are code-typed: the leak string-typed them.
        let def_line = &before[10];
        assert!(
            def_line
                .tokens
                .iter()
                .any(|t| &*t.token_type == "keyword" && t.text.trim() == "def"),
            "`def main` must be keyword-highlighted, got {:?}",
            def_line.tokens
        );
        assert!(
            def_line.tokens.iter().all(|t| &*t.token_type != "string"),
            "`def main` must not be string-typed, got {:?}",
            def_line.tokens
        );
    }

    /// Regression for the 2.16.8 large-file typing flicker: a keystroke far
    /// into a large file must relex only the affected span — here exactly the
    /// typed line — not the tail of the file, which is what overran the
    /// frame budget and dropped syntax for a frame.
    #[test]
    fn relex_bounds_typing_work_to_the_edited_line() {
        let syntax = python_syntax();
        let lines: Vec<String> = (0..2000)
            .map(|i| format!("x_{i} = {i}  # value"))
            .collect();
        let before = full_relex(&syntax, &lines.iter().map(String::as_str).collect::<Vec<_>>());

        let mut edited = lines.clone();
        edited[1499] = "x_1499 = 1501  # value".into();
        let saved: Vec<Option<Vec<u8>>> =
            before.iter().map(|l| Some(l.end_state.clone())).collect();
        let (relexed, stats) = relex_span(
            &syntax,
            &edited.iter().map(String::as_str).collect::<Vec<_>>(),
            &saved,
            1500,
        );

        // One keystroke, one line of relex work, 1,500 lines from the start
        // and 500 from the end.
        assert_eq!(stats.lines_tokenized, 1, "typing must relex one line, got {stats:?}");
        assert_eq!(stats.first_line, 1500);
        assert_eq!(stats.last_line, 1500);
        assert!(stats.stopped_early);
        assert_eq!(relexed.len(), 1);

        // And the result is the full-relex result for the new content.
        let after = full_relex(&syntax, &edited.iter().map(String::as_str).collect::<Vec<_>>());
        assert_eq!(relexed[0].tokens, after[1499].tokens);
        assert_eq!(relexed[0].end_state, after[1499].end_state);
    }

    /// Regression for the markdown emphasis leak: editing the line after an
    /// unclosed emphasis delimiter must leave the following paragraph
    /// unemphasized, matching a full relex — not carry open-emphasis state
    /// (or resurrect it from stale state) into the lines below.
    #[test]
    fn relex_does_not_bleed_markdown_emphasis_below_the_edit() {
        let syntax = markdown_syntax();
        let lines: Vec<String> = [
            "## Notes",
            "see ==foo bar baz",
            "a plain paragraph here",
            "and another one",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let before = full_relex(&syntax, &lines.iter().map(String::as_str).collect::<Vec<_>>());

        // Sanity: the unclosed opener carries no state (the pinned fix).
        assert_eq!(before[1].end_state, Vec::<u8>::new());

        let mut edited = lines.clone();
        edited[1] = "see ==foo bar baz qux".into();
        let saved: Vec<Option<Vec<u8>>> =
            before.iter().map(|l| Some(l.end_state.clone())).collect();
        let (relexed, stats) =
            relex_span(&syntax, &edited.iter().map(String::as_str).collect::<Vec<_>>(), &saved, 2);

        // Identical to a full relex of the edited content.
        let after = full_relex(&syntax, &edited.iter().map(String::as_str).collect::<Vec<_>>());
        for (n, line) in relexed.iter().enumerate() {
            assert_eq!(line.tokens, after[1 + n].tokens, "line {}", 2 + n);
        }

        // The paragraphs below stay unemphasized: no literal/bold/italic
        // tokens leak into them.
        let plain = &before[2];
        assert!(
            plain.tokens.iter().all(|t| !matches!(
                t.token_type.as_ref(),
                "literal" | "markdown_bold" | "markdown_italic" | "markdown_bold_italic"
            )),
            "emphasis bled into the following paragraph: {:?}",
            plain.tokens
        );
        assert!(stats.stopped_early, "edit must not reach below its line: {stats:?}");
    }

    #[test]
    fn full_relex_tokens_round_trip_every_line() {
        for syntax in [python_syntax(), html_syntax(), markdown_syntax()] {
            let lines: Vec<String> = python_lines()
                .into_iter()
                .chain(["<p>trailing html</p>".to_string()])
                .collect();
            let relexed = full_relex(&syntax, &lines.iter().map(String::as_str).collect::<Vec<_>>());
            for (n, line) in relexed.iter().enumerate() {
                assert_eq!(
                    joined(&line.tokens),
                    lines[n],
                    "{}: tokens must round-trip the line",
                    n + 1
                );
            }
        }
    }
}
