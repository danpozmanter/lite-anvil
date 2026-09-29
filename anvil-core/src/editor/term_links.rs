//! Pure detection of clickable spans in terminal output: URLs and the
//! `file.cpp:42:7` / `file.rs:12` / `File "app.py", line 12` forms a build
//! or test run prints.
//!
//! Detection only classifies text. Deciding "does this path exist?" and
//! opening the target is left to the caller, which knows the terminal's cwd
//! and the project root — see `resolve_location`.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermLink {
    Url(String),
    /// A source location: path, 1-based line, and 1-based column when known.
    Location {
        path: String,
        line: usize,
        col: Option<usize>,
    },
}

/// Find the link whose span contains character index `col`, if any.
pub fn link_at(text: &str, col: usize) -> Option<(TermLink, usize, usize)> {
    links_in(text)
        .into_iter()
        .find(|(_, start, end)| *start <= col && col < *end)
}

/// All clickable spans in one line of terminal text, in order, as
/// `(link, start, end)` with 0-based character indices, `end` exclusive.
pub fn links_in(text: &str) -> Vec<(TermLink, usize, usize)> {
    let mut out = Vec::new();
    for (start, token) in text
        .split_whitespace()
        .scan(0usize, |offset, token| {
            // `split_whitespace` skips runs of whitespace, so walk the byte
            // offset forward across the separators it skipped.
            let skipped = text[*offset..]
                [..text[*offset..].len() - text[*offset..].trim_start().len()]
                .len();
            *offset += skipped;
            let start = *offset;
            *offset += token.len();
            Some((start, token))
        })
    {
        let Some(link) = classify(token) else {
            continue;
        };
        // Trailing punctuation belongs to the sentence, not the link.
        let mut end = start + token.len();
        if let TermLink::Url(_) = link {
            while text[..end].ends_with(['.', ',', ';', ':', ')', ']', '}']) {
                end -= 1;
            }
        }
        out.push((link, start, end));
    }
    // Python traceback form: `  File "app.py", line 12`.
    if let Some((link, start, end)) = traceback_location(text) {
        out.push((link, start, end));
    }
    out.sort_by_key(|(_, start, _)| *start);
    out
}

/// Classify one whitespace-delimited token.
fn classify(token: &str) -> Option<TermLink> {
    if let Some(link) = url(token) {
        return Some(link);
    }
    location(token).map(|(path, line, col)| TermLink::Location { path, line, col })
}

fn url(token: &str) -> Option<TermLink> {
    let pos = token.find("://")?;
    let scheme = &token[..pos];
    let scheme_ok = !scheme.is_empty()
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    let body_ok = token[pos + 3..].chars().any(|c| !c.is_whitespace());
    (scheme_ok && body_ok).then(|| TermLink::Url(token.to_string()))
}

/// `path:line` or `path:line:col`, with the line 1-based. The path may
/// itself contain colons, so the numeric segments are peeled from the right:
/// the last is an optional column, the next is the line, the rest is the path.
fn location(token: &str) -> Option<(String, usize, Option<usize>)> {
    let mut parts: Vec<&str> = token.split(':').collect();
    if parts.len() < 2 {
        return None;
    }
    let mut col = None;
    if parts.len() >= 3 && is_number(parts[parts.len() - 1]) {
        col = parts.pop().and_then(|c| c.parse::<usize>().ok());
    }
    let line = parts.last()?.parse::<usize>().ok()?;
    parts.pop();
    let path = parts.join(":");
    if line == 0
        || path.is_empty()
        // `12:34` is a timestamp, not a file; a path needs a separator or
        // an extension dot to be worth opening.
        || (!path.contains(['.', '/']))
        || path.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    Some((path, line, col))
}

fn is_number(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

/// `File "app.py", line 12` from a Python traceback.
fn traceback_location(text: &str) -> Option<(TermLink, usize, usize)> {
    // The span starts at `File`, so a click anywhere in the frame's text —
    // including the filename — lands on the location.
    let file_start = text.find("File \"")?;
    let quote = file_start + "File \"".len();
    let close_quote = text[quote..].find('"')? + quote;
    let closing = close_quote + 1;
    let path = &text[quote..close_quote];
    if path.is_empty() {
        return None;
    }
    let rest = text[closing..].trim_start();
    let rest = rest.strip_prefix(',')?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix("line")?;
    let rest = rest.trim_start();
    let number: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if number.is_empty() {
        return None;
    }
    let line = number.parse::<usize>().ok()?;
    let end = closing + (text[closing..].len() - rest.len()) + number.len();
    Some((
        TermLink::Location {
            path: path.to_string(),
            line,
            col: None,
        },
        file_start,
        end,
    ))
}

/// Resolve a detected location against candidate base directories (the
/// terminal's cwd, the project root), keeping the first where the file
/// exists. Absolute paths are taken as-is and must exist. Locations that
/// resolve nowhere are dropped: clicking them does nothing.
pub fn resolve_location(path: &str, line: usize, col: usize, bases: &[PathBuf]) -> Option<(PathBuf, usize, usize)> {
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        return candidate.is_file().then(|| (candidate.to_path_buf(), line, col));
    }
    bases
        .iter()
        .map(|base| base.join(path))
        .find(|joined| joined.is_file())
        .map(|joined| (joined, line, col))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn location_of(text: &str) -> Option<(String, usize, Option<usize>)> {
        // Click the last character: in these messages the location is the
        // final token on the line.
        match link_at(text, text.chars().count() - 1)?.0 {
            TermLink::Location { path, line, col } => Some((path, line, col)),
            _ => None,
        }
    }

    #[test]
    fn compiler_error_file_line_col_is_a_location() {
        let text = "error[E0425]: cannot find value `x` in src/editor/main.rs:42:7";
        assert_eq!(
            location_of(text),
            Some(("src/editor/main.rs".into(), 42, Some(7)))
        );
    }

    #[test]
    fn file_line_without_column_is_a_location() {
        assert_eq!(
            location_of("warning: unused import, src/lib.rs:12"),
            Some(("src/lib.rs".into(), 12, None))
        );
    }

    #[test]
    fn python_traceback_form_is_a_location() {
        let text = "  File \"app.py\", line 12, in <module>";
        let (link, start, end) = link_at(text, 10).unwrap();
        assert_eq!(
            link,
            TermLink::Location {
                path: "app.py".into(),
                line: 12,
                col: None
            }
        );
        assert_eq!(&text[start..end], "File \"app.py\", line 12");
    }

    #[test]
    fn urls_are_links_with_sentence_punctuation_left_out() {
        let text = "see https://example.com/docs, then report.";
        match link_at(text, 6).unwrap().0 {
            TermLink::Url(url) => assert_eq!(url, "https://example.com/docs,"),
            _ => panic!("expected a URL"),
        }
        // A click on the sentence punctuation after the span finds nothing.
        let (_, _, end) = link_at(text, 6).unwrap();
        assert!(link_at(text, end).is_none());
    }

    #[test]
    fn timestamps_and_plain_words_are_not_links() {
        assert!(links_in("12:34:56 build finished").is_empty());
        assert!(links_in("cargo build --release").is_empty());
        assert!(links_in("").is_empty());
    }

    #[test]
    fn spans_contain_their_clicks() {
        let text = "src/a.rs:12 then src/b.rs:30:4";
        for col in 0..text.len() {
            let hit = link_at(text, col);
            if col < 10 {
                assert!(matches!(&hit.unwrap().0, TermLink::Location { path, .. } if path == "src/a.rs"));
            } else if (17..30).contains(&col) {
                assert!(matches!(&hit.unwrap().0, TermLink::Location { path, .. } if path == "src/b.rs"));
            }
        }
    }

    #[test]
    fn resolve_location_keeps_only_paths_that_exist() {
        let dir = std::env::temp_dir().join("anvil_term_links_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.rs"), "").unwrap();

        let bases = vec![dir.clone()];
        assert_eq!(
            resolve_location("a.rs", 3, 1, &bases),
            Some((dir.join("a.rs"), 3, 1))
        );
        assert_eq!(resolve_location("missing.rs", 3, 1, &bases), None);
        assert_eq!(
            resolve_location(&dir.join("a.rs").to_string_lossy(), 1, 1, &[]),
            Some((dir.join("a.rs"), 1, 1))
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
