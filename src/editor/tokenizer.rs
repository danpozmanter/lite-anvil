use pcre2::bytes::{Regex, RegexBuilder};
use std::collections::HashMap;
use std::sync::Arc;

use crate::editor::error::RegexError;
use crate::editor::syntax::{PatternSpec, SyntaxDefinition, TokenType};

/// A single token produced by the tokenizer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub token_type: String,
    pub text: String,
}

/// Count UTF-8 characters in a string.
pub fn char_len(text: &str) -> usize {
    text.chars().count()
}

/// 1-based character substring (inclusive on both ends).
pub fn usub(text: &str, start: usize, end: usize) -> &str {
    if start == 0 || start > end {
        return "";
    }
    let mut start_byte = None;
    let mut end_byte = None;
    let mut idx = 1usize;
    for (byte_idx, _) in text.char_indices() {
        if idx == start {
            start_byte = Some(byte_idx);
        }
        if idx == end + 1 {
            end_byte = Some(byte_idx);
            break;
        }
        idx += 1;
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

/// Append a token to a list, merging with the previous token if types match
/// or the previous token is pure whitespace.
pub fn push_token(tokens: &mut Vec<Token>, token_type: &str, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some(prev) = tokens.last_mut() {
        if prev.token_type == token_type
            || (prev.text.chars().all(char::is_whitespace) && token_type != "incomplete")
        {
            prev.token_type.clear();
            prev.token_type.push_str(token_type);
            prev.text.push_str(text);
            return;
        }
    }
    tokens.push(Token {
        token_type: token_type.to_string(),
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

/// How a single pattern matcher operates.
#[derive(Clone)]
pub enum MatcherKind {
    LuaPattern { code: String },
    Regex { compiled: Arc<Regex> },
}

/// A single pattern matcher with its anchor state.
#[derive(Clone)]
pub struct MatcherDef {
    pub kind: MatcherKind,
    pub whole_line: bool,
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
    Id(usize),
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
    builder
        .build(pattern)
        .map_err(|e| RegexError::Compile(e.to_string()))
}

/// Convert a Lua pattern string to a PCRE2 regex.
/// Expand a `%x` Lua class inside a character class `[...]`.
fn lua_class_to_regex_in_bracket(ch: char) -> &'static str {
    match ch {
        'a' => "a-zA-Z",
        'A' => "^a-zA-Z",  // only valid as first element
        'd' => "0-9",
        'D' => "^0-9",
        'w' => "a-zA-Z0-9_",
        'W' => "^a-zA-Z0-9_",
        's' => "\\s",
        'S' => "\\S",
        'l' => "a-z",
        'u' => "A-Z",
        'p' => "!-/:-@\\[-`{-~",
        _ => "",
    }
}

/// Expand a `%x` Lua class outside brackets.
fn lua_class_to_regex(ch: char) -> &'static str {
    match ch {
        'a' => "[a-zA-Z]",
        'A' => "[^a-zA-Z]",
        'd' => "\\d",
        'D' => "\\D",
        'w' => "\\w",
        'W' => "\\W",
        's' => "\\s",
        'S' => "\\S",
        'l' => "[a-z]",
        'L' => "[^a-z]",
        'u' => "[A-Z]",
        'U' => "[^A-Z]",
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
                    // %f[set] -- frontier pattern: zero-width assertion.
                    // Convert to (?=[set]) lookahead.
                    if chars.peek() == Some(&'[') {
                        chars.next(); // consume '['
                        out.push_str("(?=[");
                        // Copy bracket contents, handling %x escapes.
                        while let Some(&c) = chars.peek() {
                            if c == ']' { chars.next(); break; }
                            if c == '%' {
                                chars.next();
                                if let Some(&nc) = chars.peek() {
                                    chars.next();
                                    let expanded = lua_class_to_regex_in_bracket(nc);
                                    if expanded.is_empty() {
                                        if "\\]^-".contains(nc) { out.push('\\'); }
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
                        out.push_str("])");
                    }
                } else if next == 'b' {
                    // %bxy balanced match -- approximate.
                    if let (Some(open), Some(_close)) = (chars.next(), chars.next()) {
                        if "\\.*+?^${}()|[]".contains(open) { out.push('\\'); }
                        out.push(open);
                    }
                } else {
                    let expanded = lua_class_to_regex(next);
                    if expanded.is_empty() {
                        // Literal escape.
                        if "\\.*+?^${}()|[]".contains(next) { out.push('\\'); }
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
                if c == ']' { chars.next(); out.push(']'); break; }
                if c == '%' {
                    chars.next();
                    if let Some(&nc) = chars.peek() {
                        chars.next();
                        let expanded = lua_class_to_regex_in_bracket(nc);
                        if expanded.is_empty() {
                            if "\\]^-".contains(nc) { out.push('\\'); }
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
    let kind = match compile_regex(&regex_code) {
        Ok(compiled) => MatcherKind::Regex { compiled: Arc::new(compiled) },
        Err(_) => {
            // Fall back to storing as LuaPattern if regex compilation fails.
            MatcherKind::LuaPattern { code: regex_code }
        }
    };
    Ok(MatcherDef { kind, whole_line })
}

/// Run a regex matcher at a character offset and return 1-based character positions.
pub fn regex_find(
    matcher: &MatcherDef,
    text: &str,
    next: usize,
    anchored: bool,
) -> Result<Vec<usize>, RegexError> {
    let MatcherKind::Regex { compiled, .. } = &matcher.kind else {
        return Ok(Vec::new());
    };

    let start_byte = ucharpos(text, next)
        .unwrap_or(text.len() + 1)
        .saturating_sub(1);
    let mut locs = compiled.capture_locations();
    match compiled.captures_read_at(&mut locs, text.as_bytes(), start_byte) {
        Ok(Some(_)) => {
            let Some((s, e)) = locs.get(0) else {
                return Ok(Vec::new());
            };
            if anchored && s != start_byte {
                return Ok(Vec::new());
            }

            let mut res = vec![s + 1, e];
            for i in 1..=compiled.captures_len() {
                if let Some((cs, ce)) = locs.get(i) {
                    if cs == ce {
                        res.push(cs + 1);
                    }
                }
            }

            let char_pos_1 = if res[0] > next {
                prefix_ulen(text, res[0])
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
    let mut tokens = Vec::new();
    let line_len = char_len(line);
    if line_len == 0 {
        return tokens;
    }
    let mut pos: usize = 1;
    while pos <= line_len {
        let mut best_start = usize::MAX;
        let mut best_end: usize = 0;
        let mut best_types: Option<&[String]> = None;
        for pattern in &syntax.patterns {
            if pattern.disabled {
                continue;
            }
            match &pattern.matcher {
                PatternMatcher::Single(m) => {
                    if let Ok(results) = regex_find(m, line, pos, false) {
                        if results.len() >= 2 && results[0] >= pos && results[0] < best_start {
                            best_start = results[0];
                            best_end = results[1];
                            best_types = Some(&pattern.token_types);
                        }
                    }
                }
                PatternMatcher::Pair { open, close, .. } => {
                    if let Ok(open_res) = regex_find(open, line, pos, false) {
                        if open_res.len() >= 2 && open_res[0] >= pos && open_res[0] < best_start {
                            // Search for the close pattern after the open match.
                            let close_start = open_res[1] + 1;
                            if let Ok(close_res) = regex_find(close, line, close_start, false) {
                                if close_res.len() >= 2 {
                                    best_start = open_res[0];
                                    best_end = close_res[1];
                                    best_types = Some(&pattern.token_types);
                                }
                            }
                        }
                    }
                }
            }
        }
        if let Some(types) = best_types {
            if best_start > pos {
                push_token(&mut tokens, "normal", usub(line, pos, best_start - 1));
            }
            let token_type = types.first().map(String::as_str).unwrap_or("normal");
            let matched = usub(line, best_start, best_end);
            let mapped = syntax.symbols.get(matched).map(String::as_str).unwrap_or(token_type);
            push_token(&mut tokens, mapped, matched);
            pos = best_end + 1;
        } else {
            push_token(&mut tokens, "normal", usub(line, pos, line_len));
            break;
        }
    }
    tokens
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

        let syntax_ref = rule.syntax.as_ref().map(|s| SyntaxRef::Selector(s.clone()));

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
        assert_eq!(tokens[0].token_type, "keyword");
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
}
