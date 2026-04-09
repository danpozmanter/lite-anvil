use serde_json::Value as JsonValue;
use std::collections::HashMap;

/// A single syntax-highlighting pattern rule.
#[derive(Debug, Clone)]
pub struct PatternRule {
    /// Pattern strings: single pattern or a [open, close, escape?] pair.
    pub pattern: Option<PatternSpec>,
    /// Regex strings: single regex or a [open, close, escape?] pair.
    pub regex: Option<PatternSpec>,
    /// Token type(s) assigned to matches.
    pub token_type: TokenType,
    /// Optional sub-syntax reference.
    pub syntax: Option<String>,
}

/// Pattern specification: single string or open/close pair with optional escape.
#[derive(Debug, Clone)]
pub enum PatternSpec {
    Single(String),
    Pair {
        open: String,
        close: String,
        escape: Option<String>,
    },
}

/// Token type: a single string or multiple strings for multi-capture patterns.
#[derive(Debug, Clone)]
pub enum TokenType {
    Single(String),
    Multi(Vec<String>),
}

impl TokenType {
    /// Convenience: returns the first type name.
    pub fn first(&self) -> &str {
        match self {
            TokenType::Single(s) => s,
            TokenType::Multi(v) => v.first().map(|s| s.as_str()).unwrap_or("normal"),
        }
    }
}

/// A complete syntax definition as loaded from a JSON asset.
#[derive(Debug, Clone)]
pub struct SyntaxDefinition {
    pub name: String,
    pub files: Vec<String>,
    pub headers: Vec<String>,
    pub comment: Option<String>,
    pub block_comment: Option<(String, String)>,
    pub patterns: Vec<PatternRule>,
    pub symbols: HashMap<String, String>,
    pub space_handling: bool,
}

impl Default for SyntaxDefinition {
    fn default() -> Self {
        Self {
            name: "Plain Text".into(),
            files: Vec::new(),
            headers: Vec::new(),
            comment: None,
            block_comment: None,
            patterns: Vec::new(),
            symbols: HashMap::new(),
            space_handling: true,
        }
    }
}

/// Resolved value from the JSON graph. Mirrors the JSON structure without Lua types.
#[derive(Debug, Clone)]
pub enum GraphValue {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Array(Vec<GraphValue>),
    Object(Vec<(String, GraphValue)>),
}

impl GraphValue {
    /// Get a named field from an Object.
    pub fn get(&self, key: &str) -> Option<&GraphValue> {
        match self {
            GraphValue::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Try to interpret as a string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            GraphValue::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Try to interpret as an array.
    pub fn as_array(&self) -> Option<&[GraphValue]> {
        match self {
            GraphValue::Array(a) => Some(a),
            _ => None,
        }
    }
}

/// Resolve a JSON value with `$ref` graph references into a `GraphValue`.
pub fn resolve_graph(
    nodes: &serde_json::Map<String, JsonValue>,
    value: &JsonValue,
    cache: &mut HashMap<String, GraphValue>,
) -> Result<GraphValue, String> {
    if let Some(JsonValue::String(ref_id)) = value.get("$ref") {
        if let Some(cached) = cache.get(ref_id) {
            return Ok(cached.clone());
        }
        let node = nodes
            .get(ref_id)
            .ok_or_else(|| format!("missing graph node {ref_id}"))?;
        let kind = node
            .get("kind")
            .and_then(|k| k.as_str())
            .unwrap_or("object");

        // Insert placeholder to break cycles.
        let placeholder = if kind == "array" {
            GraphValue::Array(Vec::new())
        } else {
            GraphValue::Object(Vec::new())
        };
        cache.insert(ref_id.clone(), placeholder);

        let result = if let Some(values) = node.get("values") {
            if kind == "array" {
                if let JsonValue::Array(arr) = values {
                    let items: Result<Vec<_>, _> = arr
                        .iter()
                        .map(|item| resolve_graph(nodes, item, cache))
                        .collect();
                    GraphValue::Array(items?)
                } else {
                    GraphValue::Array(Vec::new())
                }
            } else if let JsonValue::Object(obj) = values {
                let fields: Result<Vec<_>, _> = obj
                    .iter()
                    .map(|(k, v)| resolve_graph(nodes, v, cache).map(|rv| (k.clone(), rv)))
                    .collect();
                GraphValue::Object(fields?)
            } else {
                GraphValue::Null
            }
        } else {
            GraphValue::Null
        };
        cache.insert(ref_id.clone(), result.clone());
        return Ok(result);
    }

    match value {
        JsonValue::Null => Ok(GraphValue::Null),
        JsonValue::Bool(b) => Ok(GraphValue::Bool(*b)),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(GraphValue::Int(i))
            } else {
                Ok(GraphValue::Float(n.as_f64().unwrap_or(0.0)))
            }
        }
        JsonValue::String(s) => Ok(GraphValue::Str(s.clone())),
        JsonValue::Array(arr) => {
            let items: Result<Vec<_>, _> =
                arr.iter().map(|v| resolve_graph(nodes, v, cache)).collect();
            Ok(GraphValue::Array(items?))
        }
        JsonValue::Object(obj) => {
            let fields: Result<Vec<_>, _> = obj
                .iter()
                .map(|(k, v)| resolve_graph(nodes, v, cache).map(|rv| (k.clone(), rv)))
                .collect();
            Ok(GraphValue::Object(fields?))
        }
    }
}

/// Convert a resolved `GraphValue` into a `SyntaxDefinition`.
pub fn graph_value_to_syntax(gv: &GraphValue) -> Result<SyntaxDefinition, String> {
    let mut def = SyntaxDefinition::default();

    if let Some(name) = gv.get("name").and_then(|v| v.as_str()) {
        def.name = name.to_string();
    }

    if let Some(files) = gv.get("files").and_then(|v| v.as_array()) {
        def.files = files
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
    }

    if let Some(headers) = gv.get("headers").and_then(|v| v.as_array()) {
        def.headers = headers
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
    }

    if let Some(comment) = gv.get("comment").and_then(|v| v.as_str()) {
        def.comment = Some(comment.to_string());
    }

    if let Some(bc) = gv.get("block_comment").and_then(|v| v.as_array()) {
        if bc.len() >= 2 {
            if let (Some(open), Some(close)) = (bc[0].as_str(), bc[1].as_str()) {
                def.block_comment = Some((open.to_string(), close.to_string()));
            }
        }
    }

    if let Some(GraphValue::Bool(b)) = gv.get("space_handling") {
        def.space_handling = *b;
    }

    if let Some(patterns) = gv.get("patterns").and_then(|v| v.as_array()) {
        for p in patterns {
            if let Ok(rule) = parse_pattern_rule(p) {
                def.patterns.push(rule);
            }
        }
    }

    if let Some(GraphValue::Object(fields)) = gv.get("symbols") {
        for (name, val) in fields {
            if let Some(token_type) = val.as_str() {
                def.symbols.insert(name.clone(), token_type.to_string());
            }
        }
    }

    Ok(def)
}

fn parse_pattern_rule(gv: &GraphValue) -> Result<PatternRule, String> {
    let mut rule = PatternRule {
        pattern: None,
        regex: None,
        token_type: TokenType::Single("normal".into()),
        syntax: None,
    };

    if let Some(p) = gv.get("pattern") {
        rule.pattern = Some(parse_pattern_spec(p));
    }
    if let Some(r) = gv.get("regex") {
        rule.regex = Some(parse_pattern_spec(r));
    }

    if let Some(t) = gv.get("type") {
        rule.token_type = parse_token_type(t);
    }

    if let Some(s) = gv.get("syntax").and_then(|v| v.as_str()) {
        rule.syntax = Some(s.to_string());
    }

    Ok(rule)
}

fn parse_pattern_spec(gv: &GraphValue) -> PatternSpec {
    match gv {
        GraphValue::Str(s) => PatternSpec::Single(s.clone()),
        GraphValue::Array(arr) if arr.len() >= 2 => {
            let open = arr[0].as_str().unwrap_or("").to_string();
            let close = arr[1].as_str().unwrap_or("").to_string();
            let escape = arr.get(2).and_then(|v| v.as_str()).map(String::from);
            PatternSpec::Pair {
                open,
                close,
                escape,
            }
        }
        _ => PatternSpec::Single(String::new()),
    }
}

fn parse_token_type(gv: &GraphValue) -> TokenType {
    match gv {
        GraphValue::Str(s) => TokenType::Single(s.clone()),
        GraphValue::Array(arr) => {
            let types: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            if types.len() == 1 {
                TokenType::Single(types.into_iter().next().unwrap())
            } else {
                TokenType::Multi(types)
            }
        }
        _ => TokenType::Single("normal".into()),
    }
}

/// Load all syntax definitions from JSON files in `{datadir}/assets/syntax/`.
pub fn load_syntax_assets(datadir: &str) -> Vec<SyntaxDefinition> {
    let syntax_dir = format!("{datadir}/assets/syntax");
    let entries = match std::fs::read_dir(&syntax_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut paths: Vec<_> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();

    let mut defs = Vec::new();
    for path in paths {
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if ext != "json" {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(decoded) = serde_json::from_str::<JsonValue>(&source) else {
            continue;
        };

        let payload = decoded.get("syntax").unwrap_or(&decoded);
        let gv = if let (Some(graph), Some(root)) = (payload.get("graph"), payload.get("root")) {
            let Some(nodes) = graph.get("nodes").and_then(|n| n.as_object()) else {
                continue;
            };
            let mut cache = HashMap::new();
            match resolve_graph(nodes, root, &mut cache) {
                Ok(v) => v,
                Err(_) => continue,
            }
        } else {
            let nodes = serde_json::Map::new();
            let mut cache = HashMap::new();
            match resolve_graph(&nodes, payload, &mut cache) {
                Ok(v) => v,
                Err(_) => continue,
            }
        };

        if let Ok(def) = graph_value_to_syntax(&gv) {
            defs.push(def);
        }
    }
    defs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_graph_simple_ref() {
        let json: JsonValue = serde_json::from_str(
            r#"{"nodes": {"1": {"kind": "object", "values": {"name": "Test"}}}, "root": {"$ref": "1"}}"#,
        ).unwrap();
        let nodes = json.get("nodes").unwrap().as_object().unwrap();
        let root = json.get("root").unwrap();
        let mut cache = HashMap::new();
        let gv = resolve_graph(nodes, root, &mut cache).unwrap();
        assert_eq!(gv.get("name").unwrap().as_str(), Some("Test"));
    }

    fn data_dir() -> String {
        // Tests run from the workspace or crate root; find data/ relative to the repo.
        for candidate in ["data", "../data"] {
            if std::path::Path::new(candidate)
                .join("assets/syntax")
                .is_dir()
            {
                return candidate.to_string();
            }
        }
        panic!("cannot locate data/ directory");
    }

    #[test]
    fn load_syntax_assets_finds_files() {
        let defs = load_syntax_assets(&data_dir());
        assert!(
            !defs.is_empty(),
            "should find at least one syntax definition"
        );
        let rust = defs.iter().find(|d| d.name == "Rust");
        assert!(rust.is_some(), "should find Rust syntax");
        let rust = rust.unwrap();
        assert!(!rust.files.is_empty());
        assert!(!rust.patterns.is_empty());
        assert!(!rust.symbols.is_empty());
        assert!(rust.comment.is_some());
    }

    #[test]
    fn plain_text_syntax_default() {
        let def = SyntaxDefinition::default();
        assert_eq!(def.name, "Plain Text");
        assert!(def.patterns.is_empty());
        assert!(def.symbols.is_empty());
    }

    #[test]
    fn parse_pattern_spec_single() {
        let gv = GraphValue::Str("%w+".into());
        let spec = parse_pattern_spec(&gv);
        assert!(matches!(spec, PatternSpec::Single(s) if s == "%w+"));
    }

    #[test]
    fn parse_pattern_spec_pair() {
        let gv = GraphValue::Array(vec![
            GraphValue::Str("\"".into()),
            GraphValue::Str("\"".into()),
            GraphValue::Str("\\".into()),
        ]);
        let spec = parse_pattern_spec(&gv);
        match spec {
            PatternSpec::Pair {
                open,
                close,
                escape,
            } => {
                assert_eq!(open, "\"");
                assert_eq!(close, "\"");
                assert_eq!(escape, Some("\\".into()));
            }
            _ => panic!("expected Pair"),
        }
    }

    #[test]
    fn csv_syntax_parses_correctly() {
        let defs = load_syntax_assets(&data_dir());
        let csv = defs.iter().find(|d| d.name == "CSV");
        assert!(csv.is_some());
        let csv = csv.unwrap();
        assert!(csv.files.iter().any(|f| f.contains("csv")));
        assert!(!csv.patterns.is_empty());
    }
}
