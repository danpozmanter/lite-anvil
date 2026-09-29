//! UTF-8 ↔ UTF-16 position conversion for LSP messages.
//!
//! LSP positions are UTF-16 code units; the editor's buffers and selections
//! count chars over UTF-8 `String`s. Every position crossing the protocol
//! boundary goes through one of the three helpers here, so a line with CJK
//! characters or emoji before the point maps once, correctly.

/// Convert the editor's 0-based char column to a UTF-16 column.
pub fn utf16_col(line: &str, char_col: usize) -> usize {
    let byte = line
        .char_indices()
        .nth(char_col)
        .map_or(line.len(), |(i, _)| i);
    line[..byte].encode_utf16().count()
}

/// Convert a UTF-16 column back to the editor's 0-based char column,
/// clamped to the line end.
pub fn char_col(line: &str, utf16: usize) -> usize {
    let mut units = 0usize;
    let mut chars = 0usize;
    for ch in line.chars() {
        if units >= utf16 {
            break;
        }
        units += ch.len_utf16();
        chars += 1;
    }
    chars
}

/// Convert a UTF-16 column to a byte offset into the line, clamped to the
/// line end. The byte offset is always on a char boundary.
pub fn byte_col(line: &str, utf16: usize) -> usize {
    let mut units = 0usize;
    for (byte, ch) in line.char_indices() {
        if units >= utf16 {
            return byte;
        }
        units += ch.len_utf16();
    }
    line.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    // 中 is one UTF-16 unit / three UTF-8 bytes; 😀 is two UTF-16 units /
    // four UTF-8 bytes. A file with both before the cursor is where a
    // char-column and a UTF-16 column disagree.
    const LINE: &str = "中😀tail";

    #[test]
    fn utf16_columns_on_cjk_and_emoji_lines() {
        assert_eq!(utf16_col(LINE, 0), 0); // before 中
        assert_eq!(utf16_col(LINE, 1), 1); // after 中
        assert_eq!(utf16_col(LINE, 2), 3); // after 😀: 1 + 2 units
        assert_eq!(utf16_col(LINE, 6), 7); // end of line
        assert_eq!(utf16_col(LINE, 99), 7); // clamped
        // ASCII is the identity.
        assert_eq!(utf16_col("tail", 2), 2);
    }

    #[test]
    fn utf16_columns_map_back_to_chars_and_bytes() {
        assert_eq!(char_col(LINE, 0), 0);
        assert_eq!(char_col(LINE, 1), 1); // start of 😀
        assert_eq!(char_col(LINE, 3), 2); // start of t
        assert_eq!(char_col(LINE, 7), 6); // end
        assert_eq!(char_col(LINE, 99), 6); // clamped

        assert_eq!(byte_col(LINE, 0), 0);
        assert_eq!(byte_col(LINE, 1), 3); // 中 is 3 bytes
        assert_eq!(byte_col(LINE, 3), 7); // 😀 is 4 bytes
        assert_eq!(byte_col(LINE, 7), 11); // end
        assert_eq!(byte_col(LINE, 99), 11); // clamped, on a char boundary
    }

    #[test]
    fn cursor_mapping_round_trips_through_utf16() {
        for col in 0..=LINE.chars().count() {
            assert_eq!(char_col(LINE, utf16_col(LINE, col)), col);
        }
    }
}
