use mlua::prelude::*;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use pcre2::bytes::{Regex, RegexBuilder};
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

fn json_to_lua(lua: &Lua, value: &JsonValue) -> LuaResult<LuaValue> {
    match value {
        JsonValue::Null => Ok(LuaValue::Nil),
        JsonValue::Bool(b) => Ok(LuaValue::Boolean(*b)),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(LuaValue::Integer(i))
            } else {
                Ok(LuaValue::Number(n.as_f64().unwrap_or(0.0)))
            }
        }
        JsonValue::String(s) => Ok(LuaValue::String(lua.create_string(s)?)),
        JsonValue::Array(arr) => {
            let t = lua.create_table()?;
            for (i, v) in arr.iter().enumerate() {
                t.raw_set(i as i64 + 1, json_to_lua(lua, v)?)?;
            }
            Ok(LuaValue::Table(t))
        }
        JsonValue::Object(obj) => {
            let t = lua.create_table()?;
            for (k, v) in obj {
                t.raw_set(k.as_str(), json_to_lua(lua, v)?)?;
            }
            Ok(LuaValue::Table(t))
        }
    }
}

/// Resolves a JSON value that may contain `{"$ref": "N"}` cross-references.
/// `nodes` is the `graph.nodes` map; `cache` prevents duplicate table creation.
fn resolve_graph_value(
    lua: &Lua,
    nodes: &serde_json::Map<String, JsonValue>,
    value: &JsonValue,
    cache: &mut HashMap<String, LuaTable>,
) -> LuaResult<LuaValue> {
    if let Some(JsonValue::String(ref_id)) = value.get("$ref") {
        if let Some(t) = cache.get(ref_id) {
            return Ok(LuaValue::Table(t.clone()));
        }
        let node = nodes
            .get(ref_id)
            .ok_or_else(|| LuaError::RuntimeError(format!("missing graph node {ref_id}")))?;
        let t = lua.create_table()?;
        // Pre-insert before filling to handle any cyclic refs.
        cache.insert(ref_id.clone(), t.clone());
        let kind = node
            .get("kind")
            .and_then(|k| k.as_str())
            .unwrap_or("object");
        if let Some(values) = node.get("values") {
            if kind == "array" {
                if let JsonValue::Array(arr) = values {
                    for (i, item) in arr.iter().enumerate() {
                        let v = resolve_graph_value(lua, nodes, item, cache)?;
                        t.raw_set(i as i64 + 1, v)?;
                    }
                }
            } else if let JsonValue::Object(obj) = values {
                for (k, v) in obj {
                    let resolved = resolve_graph_value(lua, nodes, v, cache)?;
                    t.raw_set(k.as_str(), resolved)?;
                }
            }
        }
        return Ok(LuaValue::Table(t));
    }
    match value {
        JsonValue::Object(obj) => {
            let t = lua.create_table()?;
            for (k, v) in obj {
                t.raw_set(k.as_str(), resolve_graph_value(lua, nodes, v, cache)?)?;
            }
            Ok(LuaValue::Table(t))
        }
        JsonValue::Array(arr) => {
            let t = lua.create_table()?;
            for (i, v) in arr.iter().enumerate() {
                t.raw_set(i as i64 + 1, resolve_graph_value(lua, nodes, v, cache)?)?;
            }
            Ok(LuaValue::Table(t))
        }
        _ => json_to_lua(lua, value),
    }
}

/// Scans `{datadir}/assets/syntax/*.json`, resolves their shared-node graph
/// format, and returns a Lua array of syntax definition tables.
fn load_assets_impl(lua: &Lua, datadir: &str) -> LuaResult<LuaTable> {
    let syntax_dir = format!("{datadir}/assets/syntax");
    let out = lua.create_table()?;
    let entries = match std::fs::read_dir(&syntax_dir) {
        Ok(e) => e,
        Err(_) => return Ok(out),
    };
    let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    let mut idx = 1i64;
    for path in paths {
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if ext != "json" {
            continue;
        }
        let source = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let decoded: JsonValue = match serde_json::from_str(&source) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let payload = decoded.get("syntax").unwrap_or(&decoded);
        let table = if let (Some(graph), Some(root)) = (payload.get("graph"), payload.get("root")) {
            let Some(nodes) = graph.get("nodes").and_then(|n| n.as_object()) else {
                continue;
            };
            let mut cache = HashMap::new();
            match resolve_graph_value(lua, nodes, root, &mut cache)? {
                LuaValue::Table(t) => t,
                _ => continue,
            }
        } else {
            match json_to_lua(lua, payload)? {
                LuaValue::Table(t) => t,
                _ => continue,
            }
        };
        out.raw_set(idx, table)?;
        idx += 1;
    }
    Ok(out)
}

#[derive(Clone)]
enum MatcherKind {
    LuaPattern { code: String },
    Regex { compiled: Arc<Regex> },
}

#[derive(Clone)]
struct MatcherDef {
    kind: MatcherKind,
    whole_line: bool,
}

#[derive(Clone)]
enum PatternMatcher {
    Single(MatcherDef),
    Pair {
        open: MatcherDef,
        close: MatcherDef,
        escape_byte: Option<u8>,
    },
}

#[derive(Clone)]
enum SyntaxRef {
    Id(usize),
    Selector(String),
}

#[derive(Clone)]
struct PatternDef {
    matcher: PatternMatcher,
    token_types: Vec<String>,
    syntax_ref: Option<SyntaxRef>,
    disabled: bool,
}

#[derive(Clone, Default)]
struct CompiledSyntax {
    patterns: Vec<PatternDef>,
    symbols: HashMap<String, String>,
}

#[derive(Clone, Default)]
struct SyntaxSummary {
    pattern_count: usize,
    symbol_count: usize,
    supported: bool,
}

#[derive(Default)]
struct SyntaxRegistry {
    next_id: usize,
    roots: HashMap<String, usize>,
    syntaxes: HashMap<usize, Arc<CompiledSyntax>>,
    table_ptrs: HashMap<usize, usize>,
}

#[derive(Clone)]
struct SyntaxState {
    current_syntax_id: usize,
    subsyntax_info: Option<(usize, usize)>,
    current_pattern_idx: usize,
    current_level: usize,
}

struct NativeTokenizerCtx {
    ufind: LuaFunction,
    syntax_get: LuaFunction,
    fps: f64,
}

static REGISTRY: Lazy<Mutex<SyntaxRegistry>> = Lazy::new(|| Mutex::new(SyntaxRegistry::default()));

fn compile_regex(pattern: &str) -> LuaResult<Regex> {
    let mut builder = RegexBuilder::new();
    builder.utf(true).ucp(true);
    builder
        .build(pattern)
        .map_err(|e| LuaError::RuntimeError(e.to_string()))
}

fn split_anchor(code: String) -> (String, bool) {
    if let Some(stripped) = code.strip_prefix('^') {
        (stripped.to_string(), true)
    } else {
        (code, false)
    }
}

fn make_matcher(kind_name: &str, code: String) -> LuaResult<MatcherDef> {
    let (code, whole_line) = split_anchor(code);
    let kind = if kind_name == "regex" {
        MatcherKind::Regex {
            compiled: Arc::new(compile_regex(&code)?),
        }
    } else {
        MatcherKind::LuaPattern { code }
    };
    Ok(MatcherDef { kind, whole_line })
}

fn first_byte(value: &str) -> Option<u8> {
    value.as_bytes().first().copied()
}

fn compile_symbols(table: Option<LuaTable>) -> LuaResult<HashMap<String, String>> {
    let mut symbols = HashMap::new();
    let Some(table) = table else {
        return Ok(symbols);
    };

    for pair in table.pairs::<String, String>() {
        let (name, token_type) = pair?;
        symbols.insert(name, token_type);
    }

    Ok(symbols)
}

fn compile_token_types(value: LuaValue) -> LuaResult<Vec<String>> {
    match value {
        LuaValue::String(s) => Ok(vec![s.to_str()?.to_string()]),
        LuaValue::Table(t) => {
            let mut out = Vec::new();
            for item in t.sequence_values::<String>() {
                out.push(item?);
            }
            Ok(out)
        }
        LuaValue::Nil => Ok(vec!["normal".to_string()]),
        _ => Err(LuaError::RuntimeError(
            "syntax pattern type must be a string or list".into(),
        )),
    }
}

fn allocate_syntax_id(registry: &mut SyntaxRegistry) -> usize {
    registry.next_id += 1;
    registry.next_id
}

fn compile_syntax_table(table: LuaTable) -> LuaResult<usize> {
    let ptr = table.to_pointer() as usize;
    {
        let registry = REGISTRY.lock();
        if let Some(id) = registry.table_ptrs.get(&ptr) {
            return Ok(*id);
        }
    }

    let id = {
        let mut registry = REGISTRY.lock();
        let id = allocate_syntax_id(&mut registry);
        registry.table_ptrs.insert(ptr, id);
        registry
            .syntaxes
            .insert(id, Arc::new(CompiledSyntax::default()));
        id
    };

    let symbols = compile_symbols(table.get::<Option<LuaTable>>("symbols")?)?;
    let mut patterns = Vec::new();

    if let Some(pattern_table) = table.get::<Option<LuaTable>>("patterns")? {
        for value in pattern_table.sequence_values::<LuaTable>() {
            let pattern = value?;
            let disabled = pattern.get::<Option<bool>>("disabled")?.unwrap_or(false);
            let token_types = compile_token_types(pattern.get::<LuaValue>("type")?)?;
            let syntax_ref = match pattern.get::<LuaValue>("syntax")? {
                LuaValue::Nil => None,
                LuaValue::String(s) => Some(SyntaxRef::Selector(s.to_str()?.to_string())),
                LuaValue::Table(t) => Some(SyntaxRef::Id(compile_syntax_table(t)?)),
                _ => {
                    return Err(LuaError::RuntimeError(
                        "syntax reference must be a string or table".into(),
                    ));
                }
            };

            let matcher = match pattern.get::<LuaValue>("pattern")? {
                LuaValue::String(s) => {
                    PatternMatcher::Single(make_matcher("pattern", s.to_str()?.to_string())?)
                }
                LuaValue::Table(t) => {
                    let open = make_matcher("pattern", t.raw_get::<String>(1)?)?;
                    let close = make_matcher("pattern", t.raw_get::<String>(2)?)?;
                    let escape_byte = t.raw_get::<Option<String>>(3)?.and_then(|s| first_byte(&s));
                    PatternMatcher::Pair {
                        open,
                        close,
                        escape_byte,
                    }
                }
                LuaValue::Nil => match pattern.get::<LuaValue>("regex")? {
                    LuaValue::String(s) => {
                        PatternMatcher::Single(make_matcher("regex", s.to_str()?.to_string())?)
                    }
                    LuaValue::Table(t) => {
                        let open = make_matcher("regex", t.raw_get::<String>(1)?)?;
                        let close = make_matcher("regex", t.raw_get::<String>(2)?)?;
                        let escape_byte =
                            t.raw_get::<Option<String>>(3)?.and_then(|s| first_byte(&s));
                        PatternMatcher::Pair {
                            open,
                            close,
                            escape_byte,
                        }
                    }
                    _ => {
                        return Err(LuaError::RuntimeError(
                            "syntax pattern requires pattern or regex".into(),
                        ));
                    }
                },
                _ => {
                    return Err(LuaError::RuntimeError(
                        "pattern field must be a string or list".into(),
                    ));
                }
            };

            patterns.push(PatternDef {
                matcher,
                token_types,
                syntax_ref,
                disabled,
            });
        }
    }

    let compiled = Arc::new(CompiledSyntax { patterns, symbols });

    let mut registry = REGISTRY.lock();
    registry.syntaxes.insert(id, compiled);
    Ok(id)
}

fn summary_for_syntax(syntax: &CompiledSyntax) -> SyntaxSummary {
    SyntaxSummary {
        pattern_count: syntax.patterns.len(),
        symbol_count: syntax.symbols.len(),
        supported: true,
    }
}

fn summary_to_lua(lua: &Lua, name: &str, summary: &SyntaxSummary) -> LuaResult<LuaTable> {
    let out = lua.create_table()?;
    out.set("name", name)?;
    out.set("pattern_count", summary.pattern_count)?;
    out.set("symbol_count", summary.symbol_count)?;
    out.set("supported", summary.supported)?;
    Ok(out)
}

fn get_ctx(lua: &Lua) -> LuaResult<NativeTokenizerCtx> {
    let string_table: LuaTable = lua.globals().get("string")?;
    let syntax_mod: LuaTable = lua
        .globals()
        .get::<LuaTable>("package")?
        .get::<LuaTable>("loaded")?
        .get("core.syntax")?;
    let config: LuaTable = lua
        .globals()
        .get::<LuaTable>("package")?
        .get::<LuaTable>("loaded")?
        .get("core.config")?;

    Ok(NativeTokenizerCtx {
        ufind: string_table.get("ufind")?,
        syntax_get: syntax_mod.get("get")?,
        fps: config.get::<f64>("fps").unwrap_or(60.0),
    })
}

fn char_len(text: &str) -> usize {
    text.chars().count()
}

fn usub(text: &str, start: usize, end: usize) -> String {
    if start == 0 || start > end {
        return String::new();
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
        return String::new();
    };
    let end_byte = end_byte.unwrap_or(text.len());
    text[start_byte..end_byte].to_string()
}

fn prefix_ulen(text: &str, byte_count: usize) -> usize {
    let clamped = byte_count.min(text.len());
    let mut end = clamped;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].chars().count()
}

fn ucharpos(text: &str, char_idx: usize) -> Option<usize> {
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

fn parse_results(values: LuaMultiValue) -> LuaResult<Vec<usize>> {
    let mut out = Vec::new();
    for value in values {
        match value {
            LuaValue::Integer(n) if n > 0 => out.push(n as usize),
            LuaValue::Number(n) if n > 0.0 => out.push(n as usize),
            LuaValue::Nil => {}
            LuaValue::Boolean(false) => {}
            LuaValue::String(s) => {
                return Err(LuaError::RuntimeError(format!(
                    "unexpected string capture from tokenizer pattern: {}",
                    s.to_str()?
                )));
            }
            other => {
                return Err(LuaError::RuntimeError(format!(
                    "unexpected tokenizer capture value: {other:?}"
                )));
            }
        }
    }
    Ok(out)
}

fn regex_find(
    matcher: &MatcherDef,
    text: &str,
    next: usize,
    anchored: bool,
) -> LuaResult<Vec<usize>> {
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
        Err(err) => Err(LuaError::RuntimeError(err.to_string())),
    }
}

fn find_text(
    ctx: &NativeTokenizerCtx,
    text: &str,
    pattern: &PatternDef,
    offset: usize,
    at_start: bool,
    close: bool,
) -> LuaResult<Vec<usize>> {
    if pattern.disabled {
        return Ok(Vec::new());
    }

    let (matcher, escape_byte) = match &pattern.matcher {
        PatternMatcher::Single(matcher) => (matcher, None),
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

    let next = offset;
    let anchored = at_start || matcher.whole_line;
    let res = match &matcher.kind {
        MatcherKind::LuaPattern { code } => {
            let pattern_code = if anchored {
                format!("^{code}")
            } else {
                code.clone()
            };
            // string.find uses byte positions; convert char→byte on input
            // and byte→char on output so the rest of the tokenizer stays
            // character-oriented.
            let byte_init = ucharpos(text, next).unwrap_or(text.len() + 1);
            let mut res = parse_results(
                ctx.ufind
                    .call::<LuaMultiValue>((text, pattern_code, byte_init))?,
            )?;
            if res.len() >= 2 {
                res[0] = prefix_ulen(text, res[0]);
                res[1] = prefix_ulen(text, res[1]);
                for item in res.iter_mut().skip(2) {
                    *item = prefix_ulen(text, item.saturating_sub(1)) + 1;
                }
            }
            res
        }
        MatcherKind::Regex { .. } => regex_find(matcher, text, next, anchored)?,
    };

    if res.is_empty() {
        return Ok(Vec::new());
    }

    if let Some(escape_byte) = escape_byte {
        let mut count = 0usize;
        let mut i = res[0].saturating_sub(1);
        while i >= 1 {
            let byte = text.as_bytes().get(i - 1).copied();
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
        return find_text(ctx, text, pattern, new_offset, at_start, close);
    }

    Ok(res)
}

type ResumeState = (Vec<(String, String)>, usize, Vec<u8>);

fn tokens_from_lua(table: LuaTable) -> LuaResult<Vec<(String, String)>> {
    let mut tokens = Vec::new();
    let len = table.raw_len();
    let mut idx = 1usize;
    while idx < len {
        let token_type: String = table.raw_get(idx)?;
        let text: String = table.raw_get(idx + 1)?;
        tokens.push((token_type, text));
        idx += 2;
    }
    Ok(tokens)
}

fn resume_from_lua(value: Option<LuaValue>) -> LuaResult<Option<ResumeState>> {
    let Some(LuaValue::Table(table)) = value else {
        return Ok(None);
    };

    let tokens = tokens_from_lua(table.get("res")?)?;
    let i = table.get::<usize>("i")?;
    let state: LuaString = table.get("state")?;
    Ok(Some((tokens, i, state.as_bytes().to_vec())))
}

fn resume_to_lua(
    lua: &Lua,
    tokens: &[(String, String)],
    i: usize,
    state: &[u8],
) -> LuaResult<LuaTable> {
    let out = lua.create_table()?;
    out.set("res", tokens_to_lua(lua, tokens)?)?;
    out.set("i", i)?;
    out.set("state", lua.create_string(state)?)?;
    Ok(out)
}

fn push_token(tokens: &mut Vec<(String, String)>, token_type: &str, text: &str) {
    if text.is_empty() {
        return;
    }

    if let Some((prev_type, prev_text)) = tokens.last_mut() {
        if prev_type == token_type
            || (prev_text.chars().all(char::is_whitespace) && token_type != "incomplete")
        {
            prev_type.clear();
            prev_type.push_str(token_type);
            prev_text.push_str(text);
            return;
        }
    }

    tokens.push((token_type.to_string(), text.to_string()));
}

fn push_tokens(
    tokens: &mut Vec<(String, String)>,
    syntax: &CompiledSyntax,
    pattern: &PatternDef,
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
                let token_type = pattern
                    .token_types
                    .get(i - 2)
                    .map(String::as_str)
                    .unwrap_or_else(|| {
                        pattern
                            .token_types
                            .first()
                            .map(String::as_str)
                            .unwrap_or("normal")
                    });
                let mapped = syntax
                    .symbols
                    .get(&text)
                    .map(String::as_str)
                    .unwrap_or(token_type);
                push_token(tokens, mapped, &text);
            }
        }
    } else if find_results.len() >= 2 {
        let start = find_results[0];
        let fin = find_results[1];
        let text = usub(full_text, start, fin);
        let token_type = pattern
            .token_types
            .first()
            .map(String::as_str)
            .unwrap_or("normal");
        let mapped = syntax
            .symbols
            .get(&text)
            .map(String::as_str)
            .unwrap_or(token_type);
        push_token(tokens, mapped, &text);
    }
}

fn resolve_syntax_id(
    _lua: &Lua,
    ctx: &NativeTokenizerCtx,
    syntax_ref: &SyntaxRef,
) -> LuaResult<usize> {
    match syntax_ref {
        SyntaxRef::Id(id) => Ok(*id),
        SyntaxRef::Selector(selector) => {
            let table: LuaTable = ctx.syntax_get.call((selector.clone(), LuaValue::Nil))?;
            compile_syntax_table(table)
        }
    }
}

fn get_syntax(id: usize) -> LuaResult<Arc<CompiledSyntax>> {
    let registry = REGISTRY.lock();
    registry
        .syntaxes
        .get(&id)
        .cloned()
        .ok_or_else(|| LuaError::RuntimeError(format!("unknown syntax id {id}")))
}

fn retrieve_syntax_state(
    lua: &Lua,
    ctx: &NativeTokenizerCtx,
    base_id: usize,
    state: &[u8],
) -> LuaResult<SyntaxState> {
    let mut current_syntax_id = base_id;
    let mut subsyntax_info = None;
    let mut current_pattern_idx = state.first().copied().unwrap_or(0) as usize;
    let mut current_level = 1usize;
    let mut current_syntax = get_syntax(current_syntax_id)?;

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
            if let Some(syntax_ref) = &pattern.syntax_ref {
                subsyntax_info = Some((current_syntax_id, target - 1));
                current_syntax_id = resolve_syntax_id(lua, ctx, syntax_ref)?;
                current_syntax = get_syntax(current_syntax_id)?;
                current_pattern_idx = 0;
                current_level = i + 2;
            } else {
                current_pattern_idx = target;
                break;
            }
        }
    }

    Ok(SyntaxState {
        current_syntax_id,
        subsyntax_info,
        current_pattern_idx,
        current_level,
    })
}

fn state_to_string(lua: &Lua, state: &[u8]) -> LuaResult<String> {
    Ok(lua.create_string(state)?.to_str()?.to_string())
}

fn tokens_to_lua(lua: &Lua, tokens: &[(String, String)]) -> LuaResult<LuaTable> {
    let out = lua.create_table_with_capacity(tokens.len() * 2, 0)?;
    for (idx, (token_type, text)) in tokens.iter().enumerate() {
        out.raw_set((idx * 2 + 1) as i64, token_type.as_str())?;
        out.raw_set((idx * 2 + 2) as i64, text.as_str())?;
    }
    Ok(out)
}

fn tokenize_impl(
    lua: &Lua,
    ctx: &NativeTokenizerCtx,
    base_id: usize,
    text: &str,
    incoming_state: Option<String>,
    resume: Option<LuaValue>,
) -> LuaResult<(LuaTable, String, LuaValue)> {
    let mut state = incoming_state
        .unwrap_or_else(|| "\0".to_string())
        .into_bytes();
    if state.is_empty() {
        state.push(0);
    }

    let base_syntax = get_syntax(base_id)?;
    if base_syntax.patterns.is_empty() {
        return Ok((
            tokens_to_lua(lua, &[(String::from("normal"), text.to_string())])?,
            state_to_string(lua, &state)?,
            LuaValue::Nil,
        ));
    }

    let mut tokens = Vec::new();
    let mut i = 1usize;
    if let Some((mut resumed, resumed_i, resumed_state)) = resume_from_lua(resume)? {
        while resumed.last().map(|(ty, _)| ty.as_str()) == Some("incomplete") {
            resumed.pop();
        }
        tokens = resumed;
        i = resumed_i;
        state = resumed_state;
    }

    let mut syntax_state = retrieve_syntax_state(lua, ctx, base_id, &state)?;
    let text_len = char_len(text);
    let start_time = Instant::now();
    let mut starting_i = i;

    while i <= text_len {
        if i.saturating_sub(starting_i) > 200 {
            starting_i = i;
            if start_time.elapsed().as_secs_f64() > 0.5 / ctx.fps.max(1.0) {
                let incomplete = usub(text, i, text_len);
                push_token(&mut tokens, "incomplete", &incomplete);
                let resume = resume_to_lua(lua, &tokens, i, &state)?;
                return Ok((
                    tokens_to_lua(lua, &tokens)?,
                    state_to_string(lua, &[0])?,
                    LuaValue::Table(resume),
                ));
            }
        }

        if syntax_state.current_pattern_idx > 0 {
            let current_syntax = get_syntax(syntax_state.current_syntax_id)?;
            let pattern_idx = syntax_state.current_pattern_idx - 1;
            if let Some(pattern) = current_syntax.patterns.get(pattern_idx) {
                let find_results = find_text(ctx, text, pattern, i, false, true)?;
                let s = find_results.first().copied();
                let e = find_results.get(1).copied();
                let token_type = pattern
                    .token_types
                    .first()
                    .map(String::as_str)
                    .unwrap_or("normal");

                let mut cont = true;
                if let Some((subsyntax_syntax_id, sub_idx)) = syntax_state.subsyntax_info {
                    let subsyntax_syntax = get_syntax(subsyntax_syntax_id)?;
                    if let Some(subsyntax_pattern) = subsyntax_syntax.patterns.get(sub_idx) {
                        let sub_find = find_text(ctx, text, subsyntax_pattern, i, false, true)?;
                        let ss = sub_find.first().copied();
                        if let Some(ss) = ss {
                            if s.is_none() || ss < s.unwrap_or(usize::MAX) {
                                let text_part = usub(text, i, ss.saturating_sub(1));
                                push_token(&mut tokens, token_type, &text_part);
                                i = ss;
                                cont = false;
                            }
                        }
                    }
                }

                if cont {
                    if let (Some(s), Some(e)) = (s, e) {
                        if s > i {
                            let text_part = usub(text, i, s - 1);
                            push_token(&mut tokens, token_type, &text_part);
                        }
                        push_tokens(&mut tokens, &current_syntax, pattern, text, find_results);
                        let state_len = state.len();
                        let idx = syntax_state.current_level - 1;
                        if idx >= state_len {
                            state.push(0);
                        }
                        state[idx] = 0;
                        syntax_state.current_pattern_idx = 0;
                        i = e + 1;
                    } else {
                        let text_part = usub(text, i, text_len);
                        push_token(&mut tokens, token_type, &text_part);
                        break;
                    }
                }
            }
        }

        while let Some((subsyntax_syntax_id, sub_idx)) = syntax_state.subsyntax_info {
            let subsyntax_syntax = get_syntax(subsyntax_syntax_id)?;
            let Some(subsyntax_pattern) = subsyntax_syntax.patterns.get(sub_idx) else {
                break;
            };
            let find_results = find_text(ctx, text, subsyntax_pattern, i, true, true)?;
            let s = find_results.first().copied();
            let e = find_results.get(1).copied();
            if let (Some(_), Some(e)) = (s, e) {
                let current_syntax = get_syntax(syntax_state.current_syntax_id)?;
                push_tokens(
                    &mut tokens,
                    &current_syntax,
                    subsyntax_pattern,
                    text,
                    find_results,
                );
                syntax_state.current_level = syntax_state.current_level.saturating_sub(1);
                state.truncate(syntax_state.current_level);
                if state.is_empty() {
                    state.push(0);
                }
                let idx = syntax_state.current_level - 1;
                if idx < state.len() {
                    state[idx] = 0;
                }
                syntax_state = retrieve_syntax_state(lua, ctx, base_id, &state)?;
                i = e + 1;
            } else {
                break;
            }
        }

        let current_syntax = get_syntax(syntax_state.current_syntax_id)?;
        let mut matched = false;
        for (n, pattern) in current_syntax.patterns.iter().enumerate() {
            let find_results = find_text(ctx, text, pattern, i, true, false)?;
            if !find_results.is_empty() {
                if find_results[0] > find_results[1] {
                    continue;
                }

                push_tokens(
                    &mut tokens,
                    &current_syntax,
                    pattern,
                    text,
                    find_results.clone(),
                );
                if matches!(pattern.matcher, PatternMatcher::Pair { .. }) {
                    if let Some(syntax_ref) = &pattern.syntax_ref {
                        let target_id = resolve_syntax_id(lua, ctx, syntax_ref)?;
                        let idx = syntax_state.current_level - 1;
                        if idx >= state.len() {
                            state.push((n + 1) as u8);
                        } else {
                            state[idx] = (n + 1) as u8;
                        }
                        syntax_state.current_level += 1;
                        syntax_state.subsyntax_info = Some((syntax_state.current_syntax_id, n));
                        syntax_state.current_syntax_id = target_id;
                        syntax_state.current_pattern_idx = 0;
                    } else {
                        let idx = syntax_state.current_level - 1;
                        if idx >= state.len() {
                            state.push((n + 1) as u8);
                        } else {
                            state[idx] = (n + 1) as u8;
                        }
                        syntax_state.current_pattern_idx = n + 1;
                    }
                }
                i = find_results[1] + 1;
                matched = true;
                break;
            }
        }

        if !matched {
            let text_part = usub(text, i, i);
            push_token(&mut tokens, "normal", &text_part);
            i += 1;
        }
    }

    Ok((
        tokens_to_lua(lua, &tokens)?,
        state_to_string(lua, &state)?,
        LuaValue::Nil,
    ))
}

pub fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    // Clear stale entries from any previous Lua VM session. table_ptrs stores
    // raw Lua table addresses; after a VM restart those addresses are freed and
    // new tables may reuse them, causing compile_syntax_table to return wrong
    // cached IDs without recompiling.
    *REGISTRY.lock() = SyntaxRegistry::default();

    let module = lua.create_table()?;

    module.set("available", lua.create_function(|_, ()| Ok(true))?)?;

    module.set(
        "load_assets",
        lua.create_function(|lua, datadir: String| load_assets_impl(lua, &datadir))?,
    )?;

    module.set(
        "register_syntax",
        lua.create_function(|lua, (name, spec): (String, LuaTable)| {
            let id = compile_syntax_table(spec)?;
            let syntax = get_syntax(id)?;
            let summary = summary_for_syntax(&syntax);
            REGISTRY.lock().roots.insert(name.clone(), id);
            summary_to_lua(lua, &name, &summary)
        })?,
    )?;

    module.set(
        "reset_syntax_cache",
        lua.create_function(|_, _name: Option<String>| {
            *REGISTRY.lock() = SyntaxRegistry::default();
            Ok(true)
        })?,
    )?;

    module.set(
        "tokenize_line",
        lua.create_function(
            |lua,
             (syntax_name, line_text, raw_state, resume): (
                String,
                String,
                LuaValue,
                Option<LuaValue>,
            )| {
                // Lua passes `false` for line 1 via `(i > 1) and prev.state`.
                // Accept any non-string as the initial state.
                let prev_state: Option<String> = match raw_state {
                    LuaValue::String(s) => Some(s.to_str()?.to_string()),
                    _ => None,
                };
                let root_id = {
                    let registry = REGISTRY.lock();
                    registry.roots.get(&syntax_name).copied()
                };
                let Some(root_id) = root_id else {
                    // Syntax not registered (e.g. plain text): return the whole
                    // line as normal rather than returning nil and forcing a fallback.
                    let tokens = tokens_to_lua(lua, &[("normal".to_string(), line_text)])?;
                    return Ok((
                        LuaValue::Table(tokens),
                        Some("\0".to_string()),
                        LuaValue::Nil,
                    ));
                };
                let ctx = get_ctx(lua)?;
                let (tokens, state, resume) =
                    tokenize_impl(lua, &ctx, root_id, &line_text, prev_state, resume)?;
                Ok((LuaValue::Table(tokens), Some(state), resume))
            },
        )?,
    )?;

    module.set(
        "get_registered_syntax",
        lua.create_function(|lua, name: String| {
            let id = {
                let registry = REGISTRY.lock();
                registry.roots.get(&name).copied()
            };
            let Some(id) = id else {
                return Ok(LuaValue::Nil);
            };
            let syntax = get_syntax(id)?;
            Ok(LuaValue::Table(summary_to_lua(
                lua,
                &name,
                &summary_for_syntax(&syntax),
            )?))
        })?,
    )?;

    module.set(
        "get_stats",
        lua.create_function(|lua, ()| {
            let registry = REGISTRY.lock();
            let out = lua.create_table()?;
            out.set("registered_syntaxes", registry.roots.len())?;
            out.set("compiled_syntaxes", registry.syntaxes.len())?;
            out.set("supported_syntaxes", registry.roots.len())?;
            Ok(out)
        })?,
    )?;

    Ok(module)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // REGISTRY is a process-global static. Tests run in parallel by default,
    // so we serialize them with this lock to prevent races on registry resets.
    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    fn setup_lua() -> LuaResult<Lua> {
        let lua = Lua::new();
        lua.load(
            r#"
            string.ufind = string.find

            package = package or {}
            package.loaded = package.loaded or {}

            package.loaded["core.config"] = { fps = 60 }

            local syntax_mod = {}
            local syntaxes = {}
            function syntax_mod.register(selector, t)
              syntaxes[selector] = t
            end
            function syntax_mod.get(selector)
              return syntaxes[selector]
            end
            package.loaded["core.syntax"] = syntax_mod
        "#,
        )
        .exec()?;
        Ok(lua)
    }

    fn register(lua: &Lua, name: &str, syntax_src: &str) -> LuaResult<usize> {
        let table: LuaTable = lua.load(syntax_src).eval()?;
        let id = compile_syntax_table(table.clone())?;
        REGISTRY.lock().roots.insert(name.to_string(), id);
        Ok(id)
    }

    #[test]
    fn tokenizes_simple_patterns() -> LuaResult<()> {
        let _guard = TEST_MUTEX.lock().unwrap();
        *REGISTRY.lock() = SyntaxRegistry::default();
        let lua = setup_lua()?;
        let id = register(
            &lua,
            "Simple",
            r#"
            return {
              name = "Simple",
              patterns = {
                { pattern = "%-%-.*", type = "comment" },
                { pattern = "[%a_][%w_]*", type = "symbol" },
              },
              symbols = { ["if"] = "keyword" },
            }
        "#,
        )?;
        let ctx = get_ctx(&lua)?;
        let (tokens, state, _) = tokenize_impl(&lua, &ctx, id, "if test", None, None)?;
        let token_vec = tokens_from_lua(tokens)?;
        assert_eq!(
            token_vec,
            vec![
                ("keyword".to_string(), "if".to_string()),
                ("symbol".to_string(), " test".to_string()),
            ]
        );
        assert_eq!(state, "\0");
        Ok(())
    }

    #[test]
    fn preserves_multiline_pair_state() -> LuaResult<()> {
        let _guard = TEST_MUTEX.lock().unwrap();
        *REGISTRY.lock() = SyntaxRegistry::default();
        let lua = setup_lua()?;
        let id = register(
            &lua,
            "Pairs",
            r#"
            return {
              name = "Pairs",
              patterns = {
                { pattern = { '"', '"', '\\' }, type = "string" },
              },
              symbols = {},
            }
        "#,
        )?;
        let ctx = get_ctx(&lua)?;
        let (first_tokens, first_state, _) = tokenize_impl(&lua, &ctx, id, "\"hello", None, None)?;
        let first_vec = tokens_from_lua(first_tokens)?;
        assert_eq!(
            first_vec,
            vec![("string".to_string(), "\"hello".to_string())]
        );
        assert_ne!(first_state, "\0");

        let (second_tokens, second_state, _) =
            tokenize_impl(&lua, &ctx, id, " world\"", Some(first_state), None)?;
        let second_vec = tokens_from_lua(second_tokens)?;
        assert_eq!(
            second_vec,
            vec![("string".to_string(), " world\"".to_string())]
        );
        assert_eq!(second_state, "\0");
        Ok(())
    }

    #[test]
    fn supports_capture_splits() -> LuaResult<()> {
        let _guard = TEST_MUTEX.lock().unwrap();
        *REGISTRY.lock() = SyntaxRegistry::default();
        let lua = setup_lua()?;
        let id = register(
            &lua,
            "Captures",
            r#"
            return {
              name = "Captures",
              patterns = {
                { pattern = "foo()bar", type = { "keyword", "normal" } },
              },
              symbols = {},
            }
        "#,
        )?;
        let ctx = get_ctx(&lua)?;
        let (tokens, _, _) = tokenize_impl(&lua, &ctx, id, "foobar", None, None)?;
        let token_vec = tokens_from_lua(tokens)?;
        assert_eq!(
            token_vec,
            vec![
                ("keyword".to_string(), "foo".to_string()),
                ("normal".to_string(), "bar".to_string()),
            ]
        );
        Ok(())
    }
}
