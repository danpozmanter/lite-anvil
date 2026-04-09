use std::sync::Arc;

use pcre2::bytes::{CaptureLocations, Regex as Pcre2Regex, RegexBuilder};

use crate::editor::error::RegexError;

/// Capture groups from a successful regex match. Each entry is an optional
/// `(start, end)` byte span. Index 0 is the whole match.
pub type CaptureGroups = Vec<Option<(usize, usize)>>;

// PCRE2 match-time option constants (same values as the Lua-facing module).
pub const ANCHORED: u32 = 0x80000000;
pub const ENDANCHORED: u32 = 0x20000000;
pub const NOTBOL: u32 = 0x00000001;
pub const NOTEOL: u32 = 0x00000002;
pub const NOTEMPTY: u32 = 0x00000004;
pub const NOTEMPTY_ATSTART: u32 = 0x00000008;

/// Compile-time option flags.
#[derive(Debug, Clone, Copy, Default)]
pub struct CompileFlags {
    pub caseless: bool,
    pub multiline: bool,
    pub dotall: bool,
}

impl CompileFlags {
    /// Parse a flags string (`"i"`, `"ms"`, etc.) into structured flags.
    pub fn from_str(flags: &str) -> Self {
        let mut f = Self::default();
        for c in flags.chars() {
            match c {
                'i' => f.caseless = true,
                'm' => f.multiline = true,
                's' => f.dotall = true,
                _ => {}
            }
        }
        f
    }
}

/// A compiled PCRE2 regex with a pure-Rust API.
#[derive(Clone)]
pub struct NativeRegex {
    inner: Arc<Pcre2Regex>,
}

impl std::fmt::Debug for NativeRegex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeRegex").finish_non_exhaustive()
    }
}

impl NativeRegex {
    /// Compile a PCRE2 pattern with UTF and UCP enabled.
    pub fn compile(pattern: &str, flags: CompileFlags) -> Result<Self, RegexError> {
        let mut b = RegexBuilder::new();
        b.utf(true).ucp(true);
        if flags.caseless {
            b.caseless(true);
        }
        if flags.multiline {
            b.multi_line(true);
        }
        if flags.dotall {
            b.dotall(true);
        }
        let re = b
            .build(pattern)
            .map_err(|e| RegexError::Compile(e.to_string()))?;
        Ok(Self {
            inner: Arc::new(re),
        })
    }

    /// Compile with a flags string (`"ims"` style).
    pub fn compile_with(pattern: &str, flags: &str) -> Result<Self, RegexError> {
        Self::compile(pattern, CompileFlags::from_str(flags))
    }

    /// Number of capture groups (excluding the whole-match group 0).
    pub fn captures_len(&self) -> usize {
        self.inner.captures_len()
    }

    /// Run a single match starting at byte offset `start`.
    /// Returns capture group byte spans as 0-based `(start, end)` pairs.
    /// Group 0 is the whole match. `None` entries are unmatched optional groups.
    pub fn captures_at(
        &self,
        subject: &[u8],
        start: usize,
    ) -> Result<Option<CaptureGroups>, RegexError> {
        let mut locs = self.inner.capture_locations();
        match self.inner.captures_read_at(&mut locs, subject, start) {
            Ok(Some(_)) => {
                let n = self.inner.captures_len() + 1;
                let groups = (0..n).map(|i| locs.get(i)).collect();
                Ok(Some(groups))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(RegexError::Match(e.to_string())),
        }
    }

    /// Run `cmatch` compatible with the Lua API: returns 1-based (start, end+1)
    /// pairs for group 0 followed by all capture groups. Empty vec means no match.
    pub fn cmatch_at(&self, subject: &[u8], start: usize) -> Result<Vec<i64>, RegexError> {
        let mut locs = self.inner.capture_locations();
        match self.inner.captures_read_at(&mut locs, subject, start) {
            Ok(Some(_)) => {
                let n = self.inner.captures_len() + 1;
                let mut out = Vec::with_capacity(n * 2);
                for i in 0..n {
                    match locs.get(i) {
                        Some((s, e)) => {
                            out.push((s + 1) as i64);
                            out.push((e + 1) as i64);
                        }
                        None => {
                            out.push(0);
                            out.push(0);
                        }
                    }
                }
                Ok(out)
            }
            Ok(None) => Ok(Vec::new()),
            Err(e) => Err(RegexError::Match(e.to_string())),
        }
    }

    /// Iterate all non-overlapping matches in `subject` starting at `start`.
    pub fn find_iter<'a>(&'a self, subject: &'a [u8], start: usize) -> FindIter<'a> {
        FindIter {
            re: self,
            subject,
            pos: start,
        }
    }

    /// Global substitution with a PCRE2-style replacement string.
    /// `$0`/`$n`/`${n}` reference capture groups, `$$` is a literal `$`.
    /// `limit == 0` means unlimited.
    pub fn gsub(
        &self,
        subject: &[u8],
        replacement: &[u8],
        limit: usize,
    ) -> Result<(Vec<u8>, usize), RegexError> {
        let mut result = Vec::with_capacity(subject.len());
        let mut count = 0usize;
        let mut pos = 0usize;

        loop {
            if limit > 0 && count >= limit {
                break;
            }
            if pos > subject.len() {
                break;
            }
            let mut locs = self.inner.capture_locations();
            match self.inner.captures_read_at(&mut locs, subject, pos) {
                Ok(Some(m)) => {
                    let ms = m.start();
                    let me = m.end();
                    result.extend_from_slice(&subject[pos..ms]);
                    result.extend_from_slice(&apply_replacement(replacement, subject, &locs));
                    count += 1;
                    if me == ms {
                        if pos < subject.len() {
                            result.push(subject[pos]);
                        }
                        pos = me + 1;
                    } else {
                        pos = me;
                    }
                }
                Ok(None) => break,
                Err(e) => return Err(RegexError::Match(e.to_string())),
            }
        }
        result.extend_from_slice(&subject[pos..]);
        Ok((result, count))
    }
}

/// Iterator over non-overlapping matches.
pub struct FindIter<'a> {
    re: &'a NativeRegex,
    subject: &'a [u8],
    pos: usize,
}

/// A single match with its capture groups (0-based byte spans).
#[derive(Debug, Clone)]
pub struct Match {
    pub groups: Vec<Option<(usize, usize)>>,
}

impl Match {
    /// Whole-match byte span (group 0). Always present on a successful match.
    pub fn span(&self) -> (usize, usize) {
        self.groups[0].expect("group 0 always present")
    }

    /// Matched text for the whole match.
    pub fn as_bytes<'a>(&self, subject: &'a [u8]) -> &'a [u8] {
        let (s, e) = self.span();
        &subject[s..e]
    }
}

impl Iterator for FindIter<'_> {
    type Item = Result<Match, RegexError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos > self.subject.len() {
            return None;
        }
        let mut locs = self.re.inner.capture_locations();
        match self
            .re
            .inner
            .captures_read_at(&mut locs, self.subject, self.pos)
        {
            Ok(Some(m)) => {
                let ms = m.start();
                let me = m.end();
                self.pos = if me == ms { me + 1 } else { me };
                let n = self.re.inner.captures_len() + 1;
                let groups = (0..n).map(|i| locs.get(i)).collect();
                Some(Ok(Match { groups }))
            }
            Ok(None) => None,
            Err(e) => Some(Err(RegexError::Match(e.to_string()))),
        }
    }
}

/// Apply PCRE2 extended replacement: `$$` -> `$`, `$0`/`$n`/`${n}` -> group.
fn apply_replacement(repl: &[u8], subject: &[u8], locs: &CaptureLocations) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < repl.len() {
        if repl[i] != b'$' || i + 1 >= repl.len() {
            out.push(repl[i]);
            i += 1;
            continue;
        }
        i += 1;
        if repl[i] == b'$' {
            out.push(b'$');
            i += 1;
        } else if repl[i] == b'{' {
            if let Some(rel_end) = repl[i + 1..].iter().position(|&b| b == b'}') {
                let key = &repl[i + 1..i + 1 + rel_end];
                i += rel_end + 2;
                if let Some(n) = std::str::from_utf8(key)
                    .ok()
                    .and_then(|s| s.parse::<usize>().ok())
                {
                    if let Some((s, e)) = locs.get(n) {
                        out.extend_from_slice(&subject[s..e]);
                    }
                }
            } else {
                out.push(b'$');
            }
        } else if repl[i].is_ascii_digit() {
            let n = (repl[i] - b'0') as usize;
            i += 1;
            if let Some((s, e)) = locs.get(n) {
                out.extend_from_slice(&subject[s..e]);
            }
        } else {
            out.push(b'$');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_and_match_simple_pattern() {
        let re = NativeRegex::compile_with("hello", "").unwrap();
        let caps = re.captures_at(b"say hello world", 0).unwrap().unwrap();
        let (s, e) = caps[0].unwrap();
        assert_eq!(&b"say hello world"[s..e], b"hello");
    }

    #[test]
    fn no_match_returns_none() {
        let re = NativeRegex::compile_with("xyz", "").unwrap();
        assert!(re.captures_at(b"hello", 0).unwrap().is_none());
    }

    #[test]
    fn caseless_flag() {
        let re = NativeRegex::compile_with("hello", "i").unwrap();
        assert!(re.captures_at(b"HELLO", 0).unwrap().is_some());
    }

    #[test]
    fn cmatch_returns_one_based_pairs() {
        let re = NativeRegex::compile_with("(h)(e)", "").unwrap();
        let pairs = re.cmatch_at(b"hello", 0).unwrap();
        // group 0: bytes 0..2 -> (1, 3), group 1: 0..1 -> (1, 2), group 2: 1..2 -> (2, 3)
        assert!(pairs.len() >= 6);
        assert_eq!(&pairs[..6], &[1, 3, 1, 2, 2, 3]);
    }

    #[test]
    fn cmatch_no_match_returns_empty() {
        let re = NativeRegex::compile_with("xyz", "").unwrap();
        let pairs = re.cmatch_at(b"hello", 0).unwrap();
        assert!(pairs.is_empty());
    }

    #[test]
    fn find_iter_collects_all_matches() {
        let re = NativeRegex::compile_with(r"\d+", "").unwrap();
        let subject = b"abc 123 def 456";
        let matches: Vec<_> = re
            .find_iter(subject, 0)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].as_bytes(subject), b"123");
        assert_eq!(matches[1].as_bytes(subject), b"456");
    }

    #[test]
    fn gsub_replaces_all() {
        let re = NativeRegex::compile_with(r"\d+", "").unwrap();
        let (result, count) = re.gsub(b"a1b2c3", b"#", 0).unwrap();
        assert_eq!(result, b"a#b#c#");
        assert_eq!(count, 3);
    }

    #[test]
    fn gsub_respects_limit() {
        let re = NativeRegex::compile_with(r"\d+", "").unwrap();
        let (result, count) = re.gsub(b"a1b2c3", b"#", 1).unwrap();
        assert_eq!(result, b"a#b2c3");
        assert_eq!(count, 1);
    }

    #[test]
    fn gsub_with_group_references() {
        let re = NativeRegex::compile_with(r"(\w+)\s+(\w+)", "").unwrap();
        let (result, _) = re.gsub(b"hello world", b"$2 $1", 0).unwrap();
        assert_eq!(result, b"world hello");
    }

    #[test]
    fn gsub_dollar_escape() {
        let re = NativeRegex::compile_with("x", "").unwrap();
        let (result, _) = re.gsub(b"x", b"$$", 0).unwrap();
        assert_eq!(result, b"$");
    }
}
