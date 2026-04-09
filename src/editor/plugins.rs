/// Escape a string for JSON-style quoting.
pub fn escape_for_quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for byte in text.bytes() {
        match byte {
            b'\\' => out.push_str("\\\\"),
            b'"' => out.push_str("\\\""),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            b'\x08' => out.push_str("\\b"),
            0x00..=0x1f | 0x7f => out.push_str(&format!("\\x{byte:02x}")),
            _ => out.push(byte as char),
        }
    }
    out.push('"');
    out
}

/// Word-wrap text to a column limit.
pub fn wordwrap_text(text: &str, limit: usize) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let mut n: usize = 0;
    for word in text.split_whitespace() {
        if n + word.len() > limit && !parts.is_empty() {
            parts.push("\n");
            n = 0;
        } else if !parts.is_empty() {
            parts.push(" ");
        }
        parts.push(word);
        n = n + word.len() + 1;
    }
    parts.concat()
}

/// Split text by a single delimiter character.
pub fn split_by_delim(text: &str, delim: char) -> Vec<String> {
    text.split(delim).map(String::from).collect()
}

/// Align columns in lines by padding with spaces at delimiter boundaries.
pub fn tabularize_lines(lines: &mut [String], delim: &str) {
    let split_char = delim.chars().next().unwrap_or(' ');
    let mut rows: Vec<Vec<String>> = Vec::with_capacity(lines.len());
    let mut col_widths: Vec<usize> = Vec::new();

    for line in lines.iter() {
        let cols = split_by_delim(line, split_char);
        for (j, col) in cols.iter().enumerate() {
            if j >= col_widths.len() {
                col_widths.push(col.len());
            } else if col.len() > col_widths[j] {
                col_widths[j] = col.len();
            }
        }
        rows.push(cols);
    }

    for row in &mut rows {
        let last = row.len().saturating_sub(1);
        for i in 0..last {
            let pad = col_widths[i].saturating_sub(row[i].len());
            row[i].extend(std::iter::repeat_n(' ', pad));
        }
    }

    for (i, row) in rows.iter().enumerate() {
        lines[i] = row.join(delim);
    }
}

/// Markdown preview layout helpers.
pub fn quote_padding(gap: f64) -> f64 {
    10.0f64.max(gap)
}

pub fn quote_trailing_padding(gap: f64) -> f64 {
    14.0f64.max(gap * 2.0)
}

pub fn quote_block_gap(gap: f64) -> f64 {
    10.0f64.max(gap)
}

pub fn list_item_gap(gap: f64) -> f64 {
    2.0f64.max((gap * 0.5).floor())
}

pub fn code_block_line_count(text: &str) -> usize {
    let with_newline = format!("{text}\n");
    let lines = with_newline.matches('\n').count();
    1.max(lines)
}

/// LSP snippet expansion types and parser.
pub mod snippet {
    /// A resolved tabstop with byte offset range in expanded text.
    #[derive(Debug, Clone)]
    pub struct Tabstop {
        pub index: u32,
        pub start: usize,
        pub end: usize,
    }

    /// Result of expanding a snippet body.
    #[derive(Debug)]
    pub struct ExpandedSnippet {
        pub text: String,
        pub tabstops: Vec<Tabstop>,
    }

    /// Expand an LSP snippet body into plain text and tabstop positions.
    pub fn expand(snippet: &str) -> ExpandedSnippet {
        let mut text = String::with_capacity(snippet.len());
        let mut tabstops = Vec::new();
        let bytes = snippet.as_bytes();
        let len = bytes.len();
        let mut i = 0;

        while i < len {
            if bytes[i] == b'\\' && i + 1 < len {
                let next = bytes[i + 1];
                if next == b'$' || next == b'\\' || next == b'}' || next == b'{' {
                    text.push(next as char);
                    i += 2;
                    continue;
                }
            }
            if bytes[i] == b'$' {
                i += 1;
                if i >= len {
                    text.push('$');
                    break;
                }
                if bytes[i] == b'{' {
                    i += 1;
                    let (index, default_text, consumed) = parse_braced(&bytes[i..]);
                    let start = text.len();
                    text.push_str(&default_text);
                    tabstops.push(Tabstop {
                        index,
                        start,
                        end: text.len(),
                    });
                    i += consumed;
                } else if bytes[i].is_ascii_digit() {
                    let start_idx = i;
                    while i < len && bytes[i].is_ascii_digit() {
                        i += 1;
                    }
                    let index: u32 = std::str::from_utf8(&bytes[start_idx..i])
                        .unwrap_or("0")
                        .parse()
                        .unwrap_or(0);
                    let pos = text.len();
                    tabstops.push(Tabstop {
                        index,
                        start: pos,
                        end: pos,
                    });
                } else {
                    text.push('$');
                }
            } else {
                text.push(bytes[i] as char);
                i += 1;
            }
        }
        tabstops.sort_by_key(|t| if t.index == 0 { u32::MAX } else { t.index });
        ExpandedSnippet { text, tabstops }
    }

    fn parse_braced(bytes: &[u8]) -> (u32, String, usize) {
        let len = bytes.len();
        let mut i = 0;
        let idx_start = i;
        while i < len && bytes[i].is_ascii_digit() {
            i += 1;
        }
        let index: u32 = std::str::from_utf8(&bytes[idx_start..i])
            .unwrap_or("0")
            .parse()
            .unwrap_or(0);

        if i >= len || bytes[i] == b'}' {
            return (index, String::new(), if i < len { i + 1 } else { i });
        }

        if bytes[i] == b'|' {
            // Choice: ${1|a,b,c|}
            i += 1;
            let choice_start = i;
            while i < len && bytes[i] != b'|' {
                i += 1;
            }
            let choices = std::str::from_utf8(&bytes[choice_start..i]).unwrap_or("");
            let first = choices.split(',').next().unwrap_or("").to_string();
            if i < len {
                i += 1; // skip |
            }
            if i < len && bytes[i] == b'}' {
                i += 1;
            }
            return (index, first, i);
        }

        if bytes[i] == b':' {
            // Default: ${1:text}
            i += 1;
            let mut default = String::new();
            let mut depth = 1u32;
            while i < len && depth > 0 {
                if bytes[i] == b'\\' && i + 1 < len {
                    default.push(bytes[i + 1] as char);
                    i += 2;
                    continue;
                }
                if bytes[i] == b'{' {
                    depth += 1;
                } else if bytes[i] == b'}' {
                    depth -= 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                }
                default.push(bytes[i] as char);
                i += 1;
            }
            return (index, default, i);
        }

        // Unknown form - skip to closing brace
        while i < len && bytes[i] != b'}' {
            i += 1;
        }
        if i < len {
            i += 1;
        }
        (index, String::new(), i)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_for_quote_basic() {
        assert_eq!(escape_for_quote("hello"), "\"hello\"");
        assert_eq!(escape_for_quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(escape_for_quote("a\nb"), "\"a\\nb\"");
    }

    #[test]
    fn wordwrap_basic() {
        let result = wordwrap_text("the quick brown fox jumps", 15);
        assert!(result.contains('\n'));
    }

    #[test]
    fn wordwrap_no_wrap_needed() {
        assert_eq!(wordwrap_text("short", 80), "short");
    }

    #[test]
    fn tabularize_aligns_columns() {
        let mut lines = vec!["a,bb,c".to_string(), "dd,e,fff".to_string()];
        tabularize_lines(&mut lines, ",");
        assert_eq!(lines[0], "a ,bb,c");
        assert_eq!(lines[1], "dd,e ,fff");
    }

    #[test]
    fn code_block_line_count_basic() {
        assert_eq!(code_block_line_count("a\nb\nc"), 3);
        assert_eq!(code_block_line_count(""), 1);
    }

    #[test]
    fn snippet_expand_basic() {
        let result = snippet::expand("hello $1 world $0");
        assert_eq!(result.text, "hello  world ");
        assert_eq!(result.tabstops.len(), 2);
        assert_eq!(result.tabstops[0].index, 1);
        assert_eq!(result.tabstops[1].index, 0);
    }

    #[test]
    fn snippet_expand_with_default() {
        let result = snippet::expand("${1:foo}bar");
        assert_eq!(result.text, "foobar");
        assert_eq!(result.tabstops[0].start, 0);
        assert_eq!(result.tabstops[0].end, 3);
    }

    #[test]
    fn snippet_expand_choice() {
        let result = snippet::expand("${1|yes,no|}");
        assert_eq!(result.text, "yes");
    }

    #[test]
    fn snippet_expand_escape() {
        let result = snippet::expand("\\$1 \\\\");
        assert_eq!(result.text, "$1 \\");
        assert!(result.tabstops.is_empty());
    }
}
