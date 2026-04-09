use crate::editor::types::Color;

/// Returns true if the byte at 1-based `offset` is a UTF-8 continuation byte.
pub fn is_utf8_cont(bytes: &[u8], offset: usize) -> bool {
    if offset == 0 || offset > bytes.len() {
        return false;
    }
    (0x80..0xc0).contains(&bytes[offset - 1])
}

/// Clamp `n` to `[lo, hi]`.
pub fn clamp(n: f64, lo: f64, hi: f64) -> f64 {
    n.min(hi).max(lo)
}

/// Round half-away-from-zero (matches Lua `common.round`).
pub fn round(n: f64) -> f64 {
    if n >= 0.0 {
        (n + 0.5).floor()
    } else {
        (n - 0.5).ceil()
    }
}

/// Linear interpolation between two numbers.
pub fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// Euclidean distance between two 2D points.
pub fn distance(x1: f64, y1: f64, x2: f64, y2: f64) -> f64 {
    ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt()
}

/// Parse a CSS-style color string into RGBA components (0-255).
/// Supports `#rrggbb`, `#rrggbbaa`, `rgb(r, g, b)`, and `rgba(r, g, b, a)`.
pub fn parse_color(input: &str) -> Result<Color, String> {
    let input = input.trim();

    if let Some(hex) = input.strip_prefix('#') {
        if (hex.len() == 6 || hex.len() == 8) && hex.chars().all(|c| c.is_ascii_hexdigit()) {
            let r = u8::from_str_radix(&hex[0..2], 16).map_err(|e| e.to_string())?;
            let g = u8::from_str_radix(&hex[2..4], 16).map_err(|e| e.to_string())?;
            let b = u8::from_str_radix(&hex[4..6], 16).map_err(|e| e.to_string())?;
            let a = if hex.len() == 8 {
                u8::from_str_radix(&hex[6..8], 16).map_err(|e| e.to_string())?
            } else {
                0xff
            };
            return Ok(Color::new(r, g, b, a));
        }
    }

    if input.starts_with("rgb") {
        let nums: Vec<f64> = input
            .chars()
            .fold((Vec::new(), String::new()), |(mut nums, mut cur), c| {
                if c.is_ascii_digit() || c == '.' {
                    cur.push(c);
                } else if !cur.is_empty() {
                    if let Ok(n) = cur.parse::<f64>() {
                        nums.push(n);
                    }
                    cur.clear();
                }
                (nums, cur)
            })
            .0;

        let r = nums.first().copied().unwrap_or(0.0) as u8;
        let g = nums.get(1).copied().unwrap_or(0.0) as u8;
        let b = nums.get(2).copied().unwrap_or(0.0) as u8;
        let a = (nums.get(3).copied().unwrap_or(1.0) * 255.0) as u8;

        return Ok(Color::new(r, g, b, a));
    }

    Err(format!("bad color string '{input}'"))
}

/// Extract the last path component (filename or directory name).
pub fn basename(path: &str, pathsep: &str) -> String {
    path.rsplit(|c: char| pathsep.contains(c))
        .find(|part| !part.is_empty())
        .unwrap_or(path)
        .to_string()
}

/// Extract the directory portion of a path, or `None` if there is no separator.
pub fn dirname(path: &str, pathsep: &str) -> Option<String> {
    let pos = path.rfind(|c: char| pathsep.contains(c))?;
    let after = &path[pos + 1..];
    if after.is_empty() || after.chars().all(|c| pathsep.contains(c)) {
        return None;
    }
    Some(path[..pos].to_string())
}

/// Replace a leading `home` prefix with `~`.
pub fn home_encode(text: &str, home: Option<&str>) -> String {
    if let Some(h) = home {
        if let Some(rest) = text.strip_prefix(h) {
            return format!("~{rest}");
        }
    }
    text.to_string()
}

/// Expand a leading `~` to `home`.
pub fn home_expand(text: &str, home: Option<&str>) -> String {
    if let Some(h) = home {
        if let Some(rest) = text.strip_prefix('~') {
            return format!("{h}{rest}");
        }
    }
    text.to_string()
}

/// Returns true if the path is absolute (Unix `/` or Windows drive letter).
pub fn is_absolute_path(path: &str, pathsep: &str) -> bool {
    if path.starts_with(pathsep) {
        return true;
    }
    let bytes = path.as_bytes();
    bytes.len() >= 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes.len() < 3 || bytes[2] == b'\\')
}

/// Returns true if `filename` starts with `path` followed by a separator.
pub fn path_belongs_to(filename: &str, path: &str, pathsep: &str) -> bool {
    let prefix = format!("{path}{pathsep}");
    filename.starts_with(&prefix)
}

/// Normalize a Windows volume prefix (uppercase drive letter, strip trailing seps).
pub fn normalize_volume(filename: &str, pathsep: &str) -> String {
    if pathsep == "\\" {
        let bytes = filename.as_bytes();
        if bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'\\'
        {
            let drive = (bytes[0] as char).to_uppercase().to_string();
            let rem = filename[3..].trim_end_matches('\\');
            return format!("{drive}:\\{rem}");
        }
    }
    filename.to_string()
}

/// Split a path on separator characters.
pub fn split_on_slash(s: &str, pathsep: &str) -> Vec<String> {
    let mut parts = Vec::new();
    if s.starts_with(|c: char| pathsep.contains(c)) {
        parts.push(String::new());
    }
    for fragment in s.split(|c: char| pathsep.contains(c)) {
        if !fragment.is_empty() {
            parts.push(fragment.to_string());
        }
    }
    parts
}

/// Normalize a file path: resolve `.` and `..` components, normalize separators.
pub fn normalize_path(filename: &str, pathsep: &str) -> Result<String, String> {
    let mut filename_str;
    let mut volume = String::new();

    if pathsep == "\\" {
        filename_str = filename.replace(['/', '\\'], "\\");
        let bytes = filename_str.as_bytes();
        if bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'\\'
        {
            volume = format!("{}:\\", (bytes[0] as char).to_uppercase());
            filename_str = filename_str[3..].to_string();
        } else if filename_str.starts_with("\\\\") {
            if let Some(end) = filename_str[2..].find('\\').and_then(|first_sep| {
                let after = first_sep + 3;
                filename_str[after..].find('\\').map(|s| after + s + 1)
            }) {
                volume = filename_str[..end].to_string();
                filename_str = filename_str[end..].to_string();
            }
        }
    } else {
        filename_str = filename.to_string();
        if filename_str.starts_with('/') {
            volume = "/".to_string();
            filename_str = filename_str[1..].to_string();
        }
    }

    let parts = split_on_slash(&filename_str, pathsep);
    let mut accu: Vec<String> = Vec::new();
    for part in &parts {
        if part == ".." {
            if !accu.is_empty() && accu.last().is_some_and(|p| p != "..") {
                accu.pop();
            } else if !volume.is_empty() {
                return Err(format!("invalid path {volume}{filename_str}"));
            } else {
                accu.push(part.clone());
            }
        } else if part != "." {
            accu.push(part.clone());
        }
    }
    let npath = accu.join(pathsep);
    if npath.is_empty() {
        Ok(format!("{volume}{pathsep}"))
    } else {
        Ok(format!("{volume}{npath}"))
    }
}

/// Compute a relative path from `ref_dir` to `dir`.
pub fn relative_path(ref_dir: &str, dir: &str, pathsep: &str) -> String {
    if pathsep == "\\" {
        let drive_of = |s: &str| -> Option<char> {
            let b = s.as_bytes();
            if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
                Some(b[0] as char)
            } else {
                None
            }
        };
        if let (Some(d1), Some(d2)) = (drive_of(dir), drive_of(ref_dir)) {
            if d1 != d2 {
                return dir.to_string();
            }
        }
    }

    let ref_parts = split_on_slash(ref_dir, pathsep);
    let dir_parts = split_on_slash(dir, pathsep);

    let mut i = 0;
    while i < ref_parts.len() && i < dir_parts.len() && ref_parts[i] == dir_parts[i] {
        i += 1;
    }

    let mut ups = String::new();
    for _ in i..ref_parts.len() {
        ups.push_str("..");
        ups.push_str(pathsep);
    }

    let rel = dir_parts[i..].join(pathsep);
    let result = format!("{ups}{rel}");
    if result.is_empty() {
        ".".to_string()
    } else {
        result
    }
}

/// Format a string as a Lua quoted literal, matching `string.format("%q", s)`.
pub fn format_lua_string(s: &str, escape: bool) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for byte in s.bytes() {
        match byte {
            b'\\' => out.push_str("\\\\"),
            b'"' => out.push_str("\\\""),
            b'\n' => {
                if escape {
                    out.push_str("\\n");
                } else {
                    out.push_str("\\\n");
                }
            }
            b'\r' => {
                if escape {
                    out.push_str("\\r");
                } else {
                    out.push_str("\\13");
                }
            }
            b'\0' => out.push_str("\\0"),
            b'\x07' => {
                if escape {
                    out.push_str("\\a");
                } else {
                    out.push_str("\\7");
                }
            }
            b'\x08' => {
                if escape {
                    out.push_str("\\b");
                } else {
                    out.push_str("\\8");
                }
            }
            b'\t' => {
                if escape {
                    out.push_str("\\t");
                } else {
                    out.push_str("\\9");
                }
            }
            b'\x0b' => {
                if escape {
                    out.push_str("\\v");
                } else {
                    out.push_str("\\11");
                }
            }
            b'\x0c' => {
                if escape {
                    out.push_str("\\f");
                } else {
                    out.push_str("\\12");
                }
            }
            b if b < 0x20 => {
                out.push_str(&format!("\\{b}"));
            }
            _ => out.push(byte as char),
        }
    }
    out.push('"');
    out
}

// ── Fuzzy match ───────────────────────────────────────────────────────────────

/// Port of the C `f_fuzzy_match` algorithm.
/// Returns `None` if needle is not a subsequence of haystack.
/// Returns `Some(score)` otherwise (higher = better match).
/// When `files=true`, matches backwards for better filename relevance.
pub fn fuzzy_match(haystack: &str, needle: &str, files: bool) -> Option<i64> {
    let hb = haystack.as_bytes();
    let nb = needle.as_bytes();
    let h_len = hb.len();
    let n_len = nb.len();
    if n_len == 0 {
        return Some(-(h_len as i64) * 10);
    }
    let mut score: i64 = 0;
    let mut run: i64 = 0;
    let mut hi: isize = if files { h_len as isize - 1 } else { 0 };
    let mut ni: isize = if files { n_len as isize - 1 } else { 0 };
    let step: isize = if files { -1 } else { 1 };
    let in_h = |i: isize| i >= 0 && i < h_len as isize;
    let in_n = |i: isize| i >= 0 && i < n_len as isize;
    while in_h(hi) && in_n(ni) {
        while in_h(hi) && hb[hi as usize] == b' ' {
            hi += step;
        }
        while in_n(ni) && nb[ni as usize] == b' ' {
            ni += step;
        }
        if !in_h(hi) || !in_n(ni) {
            break;
        }
        let hc = hb[hi as usize];
        let nc = nb[ni as usize];
        if hc.eq_ignore_ascii_case(&nc) {
            score += run * 10 - if hc != nc { 1 } else { 0 };
            run += 1;
            ni += step;
        } else {
            score -= 10;
            run = 0;
        }
        hi += step;
    }
    if in_n(ni) {
        return None;
    }
    Some(score - h_len as i64 * 10)
}

// ── Path compare ──────────────────────────────────────────────────────────────

/// Port of the C `f_path_compare` natural-sort comparison.
/// Returns `true` if path1 should sort before path2.
/// Directories sort before files; numeric segments use natural ordering.
pub fn path_compare(path1: &str, type1: &str, path2: &str, type2: &str) -> bool {
    const SEP: u8 = b'/';
    let p1 = path1.as_bytes();
    let p2 = path2.as_bytes();
    let len1 = p1.len();
    let len2 = p2.len();
    let mut t1: i32 = if type1 != "dir" { 1 } else { 0 };
    let mut t2: i32 = if type2 != "dir" { 1 } else { 0 };
    let mut offset = 0usize;
    for k in 0..len1.min(len2) {
        if p1[k] != p2[k] {
            break;
        }
        if p1[k] == SEP {
            offset = k + 1;
        }
    }
    if p1[offset..].contains(&SEP) {
        t1 = 0;
    }
    if p2[offset..].contains(&SEP) {
        t2 = 0;
    }
    if t1 != t2 {
        return t1 < t2;
    }
    let same_len = len1 == len2;
    let mut cfr: i32 = -1;
    let mut i = offset;
    let mut j = offset;
    loop {
        if i > len1 || j > len2 {
            break;
        }
        let a = if i < len1 { p1[i] } else { 0u8 };
        let b = if j < len2 { p2[j] } else { 0u8 };
        if a == 0 || b == 0 {
            if cfr < 0 {
                cfr = 0;
            }
            if !same_len {
                cfr = if a == 0 { 1 } else { 0 };
            }
            break;
        }
        if a.is_ascii_digit() && b.is_ascii_digit() {
            let mut ii = 0;
            while i + ii < len1 && p1[i + ii].is_ascii_digit() {
                ii += 1;
            }
            let mut ij = 0;
            while j + ij < len2 && p2[j + ij].is_ascii_digit() {
                ij += 1;
            }
            let mut di: u64 = 0;
            for k in 0..ii {
                di = di
                    .saturating_mul(10)
                    .saturating_add((p1[i + k] - b'0') as u64);
            }
            let mut dj: u64 = 0;
            for k in 0..ij {
                dj = dj
                    .saturating_mul(10)
                    .saturating_add((p2[j + k] - b'0') as u64);
            }
            if di != dj {
                cfr = if di < dj { 1 } else { 0 };
                break;
            }
            i += 1;
            j += 1;
            continue;
        }
        if a == b {
            i += 1;
            j += 1;
            continue;
        }
        if a == SEP || b == SEP {
            cfr = if a == SEP { 1 } else { 0 };
            break;
        }
        let al = a.to_ascii_lowercase();
        let bl = b.to_ascii_lowercase();
        if al == bl {
            if same_len && cfr < 0 {
                cfr = if a > b { 1 } else { 0 };
            }
            i += 1;
            j += 1;
            continue;
        }
        cfr = if al < bl { 1 } else { 0 };
        break;
    }
    cfr != 0
}

/// Parse "file:line:col" or "file:line" from a path string.
/// Handles Windows drive letters (C:\foo -- colon at index 1).
pub fn parse_file_location(input: &str) -> (String, Option<i64>, Option<i64>) {
    if let Some(last_colon) = input.rfind(':') {
        if last_colon == 0 {
            return (input.to_owned(), None, None);
        }
        let after = &input[last_colon + 1..];
        if let Ok(num) = after.parse::<i64>() {
            let before = &input[..last_colon];
            if let Some(second_colon) = before.rfind(':') {
                if second_colon > 0 {
                    let mid = &before[second_colon + 1..];
                    if let Ok(line) = mid.parse::<i64>() {
                        return (before[..second_colon].to_owned(), Some(line), Some(num));
                    }
                }
            }
            return (before.to_owned(), Some(num), None);
        }
    }
    (input.to_owned(), None, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_within_range() {
        assert_eq!(clamp(5.0, 0.0, 10.0), 5.0);
    }

    #[test]
    fn clamp_below() {
        assert_eq!(clamp(-1.0, 0.0, 10.0), 0.0);
    }

    #[test]
    fn clamp_above() {
        assert_eq!(clamp(15.0, 0.0, 10.0), 10.0);
    }

    #[test]
    fn round_positive() {
        assert_eq!(round(2.5), 3.0);
        assert_eq!(round(2.4), 2.0);
    }

    #[test]
    fn round_negative() {
        assert_eq!(round(-2.5), -3.0);
        assert_eq!(round(-2.4), -2.0);
    }

    #[test]
    fn lerp_midpoint() {
        assert_eq!(lerp(0.0, 10.0, 0.5), 5.0);
    }

    #[test]
    fn lerp_endpoints() {
        assert_eq!(lerp(0.0, 10.0, 0.0), 0.0);
        assert_eq!(lerp(0.0, 10.0, 1.0), 10.0);
    }

    #[test]
    fn distance_basic() {
        assert!((distance(0.0, 0.0, 3.0, 4.0) - 5.0).abs() < 1e-10);
    }

    #[test]
    fn parse_color_hex6() {
        let c = parse_color("#ff8040").unwrap();
        assert_eq!(c, Color::new(255, 128, 64, 255));
    }

    #[test]
    fn parse_color_hex8() {
        let c = parse_color("#ff804080").unwrap();
        assert_eq!(c, Color::new(255, 128, 64, 128));
    }

    #[test]
    fn parse_color_rgb() {
        let c = parse_color("rgb(255, 128, 64)").unwrap();
        assert_eq!(c, Color::new(255, 128, 64, 255));
    }

    #[test]
    fn parse_color_rgba() {
        let c = parse_color("rgba(255, 128, 64, 0.5)").unwrap();
        assert_eq!(c, Color::new(255, 128, 64, 127));
    }

    #[test]
    fn parse_color_bad_string() {
        assert!(parse_color("notacolor").is_err());
    }

    #[test]
    fn basename_unix() {
        assert_eq!(basename("/foo/bar/baz.txt", "/"), "baz.txt");
    }

    #[test]
    fn basename_trailing_slash() {
        assert_eq!(basename("/foo/bar/", "/"), "bar");
    }

    #[test]
    fn dirname_unix() {
        assert_eq!(dirname("/foo/bar/baz.txt", "/"), Some("/foo/bar".into()));
    }

    #[test]
    fn dirname_no_sep() {
        assert_eq!(dirname("file.txt", "/"), None);
    }

    #[test]
    fn home_encode_with_prefix() {
        assert_eq!(home_encode("/home/user/file", Some("/home/user")), "~/file");
    }

    #[test]
    fn home_encode_no_match() {
        assert_eq!(
            home_encode("/other/path", Some("/home/user")),
            "/other/path"
        );
    }

    #[test]
    fn home_expand_tilde() {
        assert_eq!(home_expand("~/file", Some("/home/user")), "/home/user/file");
    }

    #[test]
    fn is_absolute_unix() {
        assert!(is_absolute_path("/foo/bar", "/"));
        assert!(!is_absolute_path("foo/bar", "/"));
    }

    #[test]
    fn path_belongs_to_basic() {
        assert!(path_belongs_to("/foo/bar/baz", "/foo/bar", "/"));
        assert!(!path_belongs_to("/foo/baz", "/foo/bar", "/"));
    }

    #[test]
    fn normalize_path_resolves_dots() {
        assert_eq!(normalize_path("/foo/bar/../baz", "/").unwrap(), "/foo/baz");
    }

    #[test]
    fn normalize_path_resolves_dot() {
        assert_eq!(normalize_path("/foo/./bar", "/").unwrap(), "/foo/bar");
    }

    #[test]
    fn relative_path_sibling() {
        assert_eq!(relative_path("/foo/bar", "/foo/baz", "/"), "../baz");
    }

    #[test]
    fn relative_path_same() {
        assert_eq!(relative_path("/foo/bar", "/foo/bar", "/"), ".");
    }

    #[test]
    fn is_utf8_cont_checks_continuation_byte() {
        let s = "\u{00E9}"; // 2 bytes: 0xC3 0xA9
        let bytes = s.as_bytes();
        assert!(!is_utf8_cont(bytes, 1)); // lead byte
        assert!(is_utf8_cont(bytes, 2)); // continuation
    }

    #[test]
    fn format_lua_string_basic() {
        assert_eq!(format_lua_string("hello", false), r#""hello""#);
    }

    #[test]
    fn format_lua_string_escapes() {
        assert_eq!(format_lua_string("a\"b", false), r#""a\"b""#);
        assert_eq!(format_lua_string("a\\b", false), r#""a\\b""#);
    }

    #[test]
    fn format_lua_string_escape_mode() {
        assert_eq!(format_lua_string("a\nb", true), r#""a\nb""#);
        assert_eq!(format_lua_string("a\tb", true), r#""a\tb""#);
    }

    #[test]
    fn parse_file_location_with_line_col() {
        let (f, l, c) = parse_file_location("src/main.rs:42:10");
        assert_eq!(f, "src/main.rs");
        assert_eq!(l, Some(42));
        assert_eq!(c, Some(10));
    }

    #[test]
    fn parse_file_location_with_line() {
        let (f, l, c) = parse_file_location("src/main.rs:42");
        assert_eq!(f, "src/main.rs");
        assert_eq!(l, Some(42));
        assert_eq!(c, None);
    }

    #[test]
    fn parse_file_location_plain() {
        let (f, l, c) = parse_file_location("src/main.rs");
        assert_eq!(f, "src/main.rs");
        assert_eq!(l, None);
        assert_eq!(c, None);
    }
}
