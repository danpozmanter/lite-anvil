use crate::editor::buffer;

/// Parsed search options.
#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    pub no_case: bool,
    pub regex: bool,
    pub reverse: bool,
    pub wrap: bool,
    pub pattern: bool,
}

/// Result of a text search: 1-based line/col positions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub line1: i64,
    pub col1: i64,
    pub line2: i64,
    pub col2: i64,
}

/// Forward plain-text or regex search across lines (no Lua patterns).
/// Returns the first match at or after (line, col).
pub fn find_in_lines(
    lines: &[String],
    start_line: i64,
    start_col: i64,
    text: &str,
    opts: &SearchOptions,
) -> Option<SearchResult> {
    let num_lines = lines.len() as i64;
    if num_lines == 0 || text.is_empty() {
        return None;
    }

    let needle = if opts.no_case && !opts.regex {
        text.to_lowercase()
    } else {
        text.to_string()
    };

    for l in start_line..=num_lines {
        let col = if l == start_line { start_col } else { 1 };
        let line = &lines[(l - 1) as usize];
        if let Some(result) = search_in_line(line, &needle, col, l, num_lines, opts) {
            return Some(result);
        }
    }
    None
}

/// Search within a single line starting at `col` (1-based).
fn search_in_line(
    line: &str,
    needle: &str,
    col: i64,
    line_idx: i64,
    num_lines: i64,
    opts: &SearchOptions,
) -> Option<SearchResult> {
    let hay = if opts.no_case && !opts.regex {
        line.to_lowercase()
    } else {
        line.to_string()
    };

    if opts.regex {
        let (s, e) = buffer::regex_find_in_line(&hay, needle, opts.no_case, col as usize)?;
        let mut line2 = line_idx;
        let mut end_col = e as i64;
        if e >= line.len() {
            line2 = line_idx + 1;
            end_col = 1;
        }
        if line2 <= num_lines {
            return Some(SearchResult {
                line1: line_idx,
                col1: s as i64,
                line2,
                col2: end_col,
            });
        }
    } else {
        let start = (col as usize).saturating_sub(1);
        if start < hay.len() {
            if let Some(offset) = hay[start..].find(needle) {
                let s = start + offset + 1;
                let e = s + needle.len();
                let mut line2 = line_idx;
                let mut end_col = e as i64;
                if e > line.len() {
                    line2 = line_idx + 1;
                    end_col = 1;
                }
                if line2 <= num_lines {
                    return Some(SearchResult {
                        line1: line_idx,
                        col1: s as i64,
                        line2,
                        col2: end_col,
                    });
                }
            }
        }
    }
    None
}

/// Returns true if `ch` is a non-word character according to `non_word_chars`.
pub fn is_non_word(ch: &[u8], non_word_chars: &[u8]) -> bool {
    non_word_chars.windows(ch.len()).any(|w| w == ch)
}

/// Find the indentation end column (1-based) for a line.
/// If the cursor is past indentation, return the indent end; otherwise return 1.
pub fn start_of_indentation(line_text: &str, col: i64) -> i64 {
    let indent_end = line_text
        .find(|c: char| !c.is_whitespace())
        .map(|i| i as i64 + 1)
        .unwrap_or(1);
    if col > indent_end { indent_end } else { 1 }
}

/// Navigate backward to the start of a paragraph/block.
/// Returns (line, col) 1-based.
pub fn previous_block_start(lines: &[String], line: i64) -> (i64, i64) {
    let mut l = line;
    loop {
        l -= 1;
        if l <= 1 {
            return (1, 1);
        }
        let prev_line = &lines[(l - 2) as usize];
        let cur_line = &lines[(l - 1) as usize];
        let prev_blank = prev_line.trim().is_empty();
        let cur_blank = cur_line.trim().is_empty();
        if prev_blank && !cur_blank {
            let first_non_ws = cur_line
                .find(|c: char| !c.is_whitespace())
                .map(|i| i as i64 + 1)
                .unwrap_or(1);
            return (l, first_non_ws);
        }
    }
}

/// Navigate forward to the end of a paragraph/block.
/// Returns (line, col) 1-based.
pub fn next_block_end(lines: &[String], line: i64) -> (i64, i64) {
    let num_lines = lines.len() as i64;
    let mut l = line;
    loop {
        if l >= num_lines {
            let last_line = &lines[(num_lines - 1) as usize];
            return (num_lines, last_line.len() as i64);
        }
        let next_line = &lines[l as usize];
        let cur_line = &lines[(l - 1) as usize];
        let next_blank = next_line.trim().is_empty();
        let cur_blank = cur_line.trim().is_empty();
        if next_blank && !cur_blank {
            return (l + 1, next_line.len() as i64);
        }
        l += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_in_lines_plain() {
        let lines = vec!["hello world\n".to_string(), "foo bar\n".to_string()];
        let result = find_in_lines(&lines, 1, 1, "bar", &SearchOptions::default());
        assert_eq!(
            result,
            Some(SearchResult {
                line1: 2,
                col1: 5,
                line2: 2,
                col2: 8,
            })
        );
    }

    #[test]
    fn find_in_lines_no_case() {
        let lines = vec!["Hello World\n".to_string()];
        let opts = SearchOptions {
            no_case: true,
            ..Default::default()
        };
        let result = find_in_lines(&lines, 1, 1, "hello", &opts);
        assert!(result.is_some());
        assert_eq!(result.unwrap().col1, 1);
    }

    #[test]
    fn find_in_lines_from_offset() {
        let lines = vec!["aa bb aa\n".to_string()];
        let result = find_in_lines(&lines, 1, 4, "aa", &SearchOptions::default());
        assert_eq!(
            result,
            Some(SearchResult {
                line1: 1,
                col1: 7,
                line2: 1,
                col2: 9,
            })
        );
    }

    #[test]
    fn find_in_lines_not_found() {
        let lines = vec!["hello\n".to_string()];
        let result = find_in_lines(&lines, 1, 1, "xyz", &SearchOptions::default());
        assert!(result.is_none());
    }

    #[test]
    fn find_in_lines_regex() {
        let lines = vec!["abc 123 def\n".to_string()];
        let opts = SearchOptions {
            regex: true,
            ..Default::default()
        };
        let result = find_in_lines(&lines, 1, 1, r"\d+", &opts);
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(r.col1, 5);
        assert_eq!(r.col2, 8);
    }

    #[test]
    fn is_non_word_basic() {
        let nw = b" \t\n/\\()\"':,.;<>~!@#$%^&*|+=[]{}`?-";
        assert!(is_non_word(b" ", nw));
        assert!(is_non_word(b"+", nw));
        assert!(!is_non_word(b"a", nw));
    }

    #[test]
    fn start_of_indentation_past_indent() {
        assert_eq!(start_of_indentation("  hello", 10), 3);
    }

    #[test]
    fn start_of_indentation_at_indent() {
        assert_eq!(start_of_indentation("  hello", 2), 1);
    }

    #[test]
    fn previous_block_start_basic() {
        let lines = vec![
            "first\n".to_string(),
            "\n".to_string(),
            "  second\n".to_string(),
            "third\n".to_string(),
        ];
        let (l, c) = previous_block_start(&lines, 4);
        assert_eq!(l, 3);
        assert_eq!(c, 3); // first non-ws in "  second\n"
    }

    #[test]
    fn next_block_end_basic() {
        let lines = vec![
            "first\n".to_string(),
            "second\n".to_string(),
            "\n".to_string(),
            "third\n".to_string(),
        ];
        let (l, _c) = next_block_end(&lines, 1);
        assert_eq!(l, 3);
    }
}
