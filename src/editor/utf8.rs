/// Byte length of a single UTF-8 character from its lead byte.
pub fn char_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b < 0xE0 {
        2
    } else if b < 0xF0 {
        3
    } else {
        4
    }
}

/// Returns true if the byte is a UTF-8 continuation byte.
pub fn is_continuation(b: u8) -> bool {
    (b & 0xC0) == 0x80
}

/// Count UTF-8 characters in a byte slice.
pub fn count_chars(bytes: &[u8]) -> usize {
    bytes.iter().filter(|&&b| !is_continuation(b)).count()
}

/// Count UTF-8 characters in `bytes[start..=end]` (0-based byte indices).
pub fn count_chars_range(bytes: &[u8], start: usize, end: usize) -> usize {
    if start > end || start >= bytes.len() {
        return 0;
    }
    let end = end.min(bytes.len() - 1);
    bytes[start..=end]
        .iter()
        .filter(|&&b| !is_continuation(b))
        .count()
}

/// Count UTF-8 characters in `bytes[i-1..j]` where `i` and `j` are 1-based
/// byte indices (inclusive). Negative indices wrap from the end.
/// Matches the Lua `utf8.len(s [, i [, j]])` semantics.
pub fn len(bytes: &[u8], i: Option<i64>, j: Option<i64>) -> usize {
    let blen = bytes.len() as i64;
    let mut i = i.unwrap_or(1);
    let mut j = j.unwrap_or(blen);
    if i < 0 {
        i = blen + i + 1;
    }
    if j < 0 {
        j = blen + j + 1;
    }
    i = i.max(1);
    j = j.min(blen);
    if i > j {
        return 0;
    }
    bytes[(i as usize - 1)..j as usize]
        .iter()
        .filter(|&&b| !is_continuation(b))
        .count()
}

/// 0-based byte offset of the `n`-th UTF-8 character (1-based char index).
/// Returns `None` if `n` is beyond the string length.
pub fn char_to_byte(bytes: &[u8], n: usize) -> Option<usize> {
    if n == 0 {
        return Some(0);
    }
    let mut count = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        if !is_continuation(b) {
            count += 1;
            if count == n {
                return Some(i);
            }
        }
    }
    None
}

/// Substring by 1-based character indices (inclusive, negative wraps from end).
pub fn sub(s: &str, i: i64, j: Option<i64>) -> &str {
    let bytes = s.as_bytes();
    let nchars = count_chars(bytes) as i64;
    let mut i = if i < 0 { nchars + i + 1 } else { i };
    let mut j = match j {
        Some(j) => {
            if j < 0 {
                nchars + j + 1
            } else {
                j
            }
        }
        None => nchars,
    };
    i = i.max(1);
    j = j.min(nchars);
    if i > j {
        return "";
    }
    let bi = match char_to_byte(bytes, i as usize) {
        Some(p) => p,
        None => return "",
    };
    let bj_end = match char_to_byte(bytes, (j + 1) as usize) {
        Some(p) => p,
        None => bytes.len(),
    };
    &s[bi..bj_end]
}

/// Reverse a UTF-8 string character-by-character.
pub fn reverse(s: &str) -> String {
    s.chars().rev().collect()
}

/// Decode the codepoint at byte position `pos` (0-based).
/// Returns `(codepoint, byte_length)`.
pub fn codepoint_at(bytes: &[u8], pos: usize) -> Option<(u32, usize)> {
    if pos >= bytes.len() {
        return None;
    }
    let b = bytes[pos];
    let (cp, clen) = match char_len(b) {
        1 => (b as u32, 1),
        2 => {
            let b2 = *bytes.get(pos + 1)?;
            (((b as u32 & 0x1F) << 6) | (b2 as u32 & 0x3F), 2)
        }
        3 => {
            let b2 = *bytes.get(pos + 1)?;
            let b3 = *bytes.get(pos + 2)?;
            (
                ((b as u32 & 0x0F) << 12) | ((b2 as u32 & 0x3F) << 6) | (b3 as u32 & 0x3F),
                3,
            )
        }
        _ => {
            let b2 = *bytes.get(pos + 1)?;
            let b3 = *bytes.get(pos + 2)?;
            let b4 = *bytes.get(pos + 3)?;
            (
                ((b as u32 & 0x07) << 18)
                    | ((b2 as u32 & 0x3F) << 12)
                    | ((b3 as u32 & 0x3F) << 6)
                    | (b4 as u32 & 0x3F),
                4,
            )
        }
    };
    Some((cp, clen))
}

/// Advance past the character at 1-based byte position `pos`.
/// Returns `(end_byte_pos_1based, codepoint)`, or `None` if past the end.
/// Matches the `utf8extra.next(s, pos)` Lua semantics.
pub fn next(bytes: &[u8], pos: Option<i64>) -> Option<(i64, u32)> {
    let pos = (pos.unwrap_or(0) + 1) as usize;
    if pos == 0 || pos > bytes.len() {
        return None;
    }
    let idx = pos - 1;
    let (cp, clen) = codepoint_at(bytes, idx)?;
    Some(((idx + clen) as i64, cp))
}

/// Insert `value` at 1-based character position `offset` in `s`.
pub fn insert(s: &str, offset: usize, value: &str) -> String {
    let bytes = s.as_bytes();
    match char_to_byte(bytes, offset) {
        Some(bi) => {
            let mut result = String::with_capacity(s.len() + value.len());
            result.push_str(&s[..bi]);
            result.push_str(value);
            result.push_str(&s[bi..]);
            result
        }
        None => {
            let mut result = String::with_capacity(s.len() + value.len());
            result.push_str(s);
            result.push_str(value);
            result
        }
    }
}

/// Remove characters from 1-based `start` through `end` (inclusive).
pub fn remove(s: &str, start: usize, end: Option<usize>) -> String {
    let bytes = s.as_bytes();
    let end = end.unwrap_or(start);
    let bi = match char_to_byte(bytes, start) {
        Some(p) => p,
        None => return s.to_string(),
    };
    let bj_next = match char_to_byte(bytes, end + 1) {
        Some(p) => p,
        None => bytes.len(),
    };
    let mut result = String::with_capacity(s.len() - (bj_next - bi));
    result.push_str(&s[..bi]);
    result.push_str(&s[bj_next..]);
    result
}

/// Simplified character width: every character counts as 1.
pub fn width(s: &str) -> usize {
    count_chars(s.as_bytes())
}

/// Byte position (1-based) of the `w`-th character (1-based).
/// Returns `(byte_pos_or_none, w)`.
pub fn widthindex(bytes: &[u8], w: i64) -> (Option<usize>, i64) {
    let pos = char_to_byte(bytes, w as usize).map(|p| p + 1);
    (pos, w)
}

/// Lowercase a UTF-8 string.
pub fn lower(s: &str) -> String {
    s.to_lowercase()
}

/// Uppercase a UTF-8 string.
pub fn upper(s: &str) -> String {
    s.to_uppercase()
}

/// Titlecase: uppercase first character, lowercase the rest.
pub fn title(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => {
            let mut result = first.to_uppercase().to_string();
            for ch in chars {
                for lc in ch.to_lowercase() {
                    result.push(lc);
                }
            }
            result
        }
    }
}

/// Case-fold (lowercase for simple folding).
pub fn fold(s: &str) -> String {
    s.to_lowercase()
}

/// Case-insensitive comparison. Returns -1, 0, or 1.
pub fn ncasecmp(a: &str, b: &str) -> i64 {
    let la = a.to_lowercase();
    let lb = b.to_lowercase();
    match la.cmp(&lb) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }
}

/// Convert `\{XXXX}` hex escape sequences to UTF-8 characters.
pub fn escape(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'\\' && bytes[i + 1] == b'{' {
            if let Some(close) = bytes[i + 2..].iter().position(|&b| b == b'}') {
                let hex = &s[i + 2..i + 2 + close];
                if let Ok(cp) = u32::from_str_radix(hex, 16) {
                    if let Some(ch) = char::from_u32(cp) {
                        result.push(ch);
                        i += 2 + close + 1;
                        continue;
                    }
                }
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_ascii() {
        assert_eq!(count_chars(b"hello"), 5);
    }

    #[test]
    fn count_multibyte() {
        assert_eq!(count_chars("cafe\u{0301}".as_bytes()), 5);
        assert_eq!(count_chars("\u{1F600}".as_bytes()), 1);
    }

    #[test]
    fn len_defaults() {
        assert_eq!(len(b"hello", None, None), 5);
    }

    #[test]
    fn len_with_range() {
        assert_eq!(len(b"hello", Some(2), Some(4)), 3);
    }

    #[test]
    fn len_negative_indices() {
        assert_eq!(len(b"hello", Some(-3), None), 3);
    }

    #[test]
    fn sub_basic() {
        assert_eq!(sub("hello", 2, Some(4)), "ell");
    }

    #[test]
    fn sub_negative_indices() {
        assert_eq!(sub("hello", -3, None), "llo");
    }

    #[test]
    fn sub_multibyte() {
        let s = "\u{00E9}\u{00E8}\u{00EA}";
        assert_eq!(sub(s, 2, Some(2)), "\u{00E8}");
    }

    #[test]
    fn reverse_ascii() {
        assert_eq!(reverse("hello"), "olleh");
    }

    #[test]
    fn reverse_multibyte() {
        assert_eq!(reverse("\u{00E9}\u{00E8}"), "\u{00E8}\u{00E9}");
    }

    #[test]
    fn insert_at_position() {
        assert_eq!(insert("hello", 3, "XY"), "heXYllo");
    }

    #[test]
    fn insert_past_end() {
        assert_eq!(insert("hi", 99, "!"), "hi!");
    }

    #[test]
    fn remove_single() {
        assert_eq!(remove("hello", 2, None), "hllo");
    }

    #[test]
    fn remove_range() {
        assert_eq!(remove("hello", 2, Some(4)), "ho");
    }

    #[test]
    fn codepoint_ascii() {
        let (cp, clen) = codepoint_at(b"A", 0).unwrap();
        assert_eq!(cp, 65);
        assert_eq!(clen, 1);
    }

    #[test]
    fn codepoint_multibyte() {
        let bytes = "\u{00E9}".as_bytes();
        let (cp, clen) = codepoint_at(bytes, 0).unwrap();
        assert_eq!(cp, 0x00E9);
        assert_eq!(clen, 2);
    }

    #[test]
    fn next_advances_through_string() {
        let s = "A\u{00E9}B";
        let bytes = s.as_bytes();
        let (end1, cp1) = next(bytes, None).unwrap();
        assert_eq!(cp1, 65); // 'A'
        assert_eq!(end1, 1);
        let (end2, cp2) = next(bytes, Some(end1)).unwrap();
        assert_eq!(cp2, 0xE9);
        assert_eq!(end2, 3); // 2 bytes for e-acute
        let (end3, cp3) = next(bytes, Some(end2)).unwrap();
        assert_eq!(cp3, 66); // 'B'
        assert_eq!(end3, 4);
        assert!(next(bytes, Some(end3)).is_none());
    }

    #[test]
    fn lower_and_upper() {
        assert_eq!(lower("Hello"), "hello");
        assert_eq!(upper("Hello"), "HELLO");
    }

    #[test]
    fn title_basic() {
        assert_eq!(title("hello"), "Hello");
        assert_eq!(title(""), "");
    }

    #[test]
    fn ncasecmp_ordering() {
        assert_eq!(ncasecmp("abc", "ABC"), 0);
        assert_eq!(ncasecmp("abc", "def"), -1);
        assert_eq!(ncasecmp("def", "abc"), 1);
    }

    #[test]
    fn widthindex_basic() {
        let (pos, w) = widthindex(b"hello", 3);
        assert_eq!(pos, Some(3)); // 1-based byte pos of 3rd char
        assert_eq!(w, 3);
    }

    #[test]
    fn escape_hex_sequences() {
        assert_eq!(escape("\\{41}"), "A");
        assert_eq!(escape("a\\{E9}b"), "a\u{00E9}b");
        assert_eq!(escape("no escape"), "no escape");
    }
}
