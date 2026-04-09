use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::collections::{BTreeSet, HashMap, HashSet};

static SYMBOLS: Lazy<Mutex<HashMap<u64, Vec<String>>>> = Lazy::new(|| Mutex::new(HashMap::new()));

/// Scan lines for identifier-like symbols (alphanumeric + underscore).
/// Returns (sorted_symbols, exceeded_limit).
pub fn scan_symbols(
    lines: &[String],
    max_symbols: usize,
    excluded: &HashSet<String>,
) -> (Vec<String>, bool) {
    let mut seen = BTreeSet::new();
    for line in lines {
        let bytes = line.as_bytes();
        let mut idx = 0usize;
        while idx < bytes.len() {
            let ch = bytes[idx];
            if ch != b'_' && !ch.is_ascii_alphabetic() {
                idx += 1;
                continue;
            }
            let start = idx;
            idx += 1;
            while idx < bytes.len() && (bytes[idx] == b'_' || bytes[idx].is_ascii_alphanumeric()) {
                idx += 1;
            }
            let sym = &line[start..idx];
            if !excluded.contains(sym) {
                seen.insert(sym.to_string());
                if seen.len() > max_symbols {
                    return (Vec::new(), true);
                }
            }
        }
    }
    (seen.into_iter().collect(), false)
}

/// Store symbols for a document. Returns (count, exceeded).
pub fn set_doc_symbols(
    doc_id: u64,
    lines: &[String],
    max_symbols: usize,
    excluded: &HashSet<String>,
) -> (usize, bool) {
    let (symbols, exceeded) = scan_symbols(lines, max_symbols, excluded);
    let count = symbols.len();
    if !exceeded {
        SYMBOLS.lock().insert(doc_id, symbols);
    } else {
        SYMBOLS.lock().remove(&doc_id);
    }
    (count, exceeded)
}

/// Remove symbols for a document.
pub fn remove_doc(doc_id: u64) -> bool {
    SYMBOLS.lock().remove(&doc_id).is_some()
}

/// Get symbols for a document.
pub fn get_doc_symbols(doc_id: u64) -> Vec<String> {
    SYMBOLS.lock().get(&doc_id).cloned().unwrap_or_default()
}

/// Collect and merge symbols from multiple documents.
pub fn collect(doc_ids: &[u64]) -> Vec<String> {
    let mut merged = BTreeSet::new();
    let guard = SYMBOLS.lock();
    for &doc_id in doc_ids {
        if let Some(symbols) = guard.get(&doc_id) {
            for sym in symbols {
                merged.insert(sym.clone());
            }
        }
    }
    merged.into_iter().collect()
}

/// Clear all stored symbols.
pub fn clear_all() {
    let mut guard = SYMBOLS.lock();
    guard.clear();
    guard.shrink_to_fit();
}

/// Shrink symbol storage.
pub fn shrink() {
    SYMBOLS.lock().shrink_to_fit();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_symbols_basic() {
        let lines = vec!["let foo = bar + baz;\n".to_string()];
        let (syms, exceeded) = scan_symbols(&lines, 100, &HashSet::new());
        assert!(!exceeded);
        assert!(syms.contains(&"foo".to_string()));
        assert!(syms.contains(&"bar".to_string()));
        assert!(syms.contains(&"baz".to_string()));
        assert!(syms.contains(&"let".to_string()));
    }

    #[test]
    fn scan_symbols_excludes() {
        let lines = vec!["let foo = bar;\n".to_string()];
        let excluded: HashSet<String> = ["let".to_string()].into();
        let (syms, _) = scan_symbols(&lines, 100, &excluded);
        assert!(!syms.contains(&"let".to_string()));
        assert!(syms.contains(&"foo".to_string()));
    }

    #[test]
    fn scan_symbols_exceeds_limit() {
        let lines = vec!["a b c d e f g h i j k\n".to_string()];
        let (syms, exceeded) = scan_symbols(&lines, 5, &HashSet::new());
        assert!(exceeded);
        assert!(syms.is_empty());
    }

    #[test]
    fn set_get_remove_round_trip() {
        let doc_id = 999999;
        let lines = vec!["alpha beta\n".to_string()];
        set_doc_symbols(doc_id, &lines, 100, &HashSet::new());
        let syms = get_doc_symbols(doc_id);
        assert!(syms.contains(&"alpha".to_string()));
        assert!(remove_doc(doc_id));
        assert!(get_doc_symbols(doc_id).is_empty());
    }

    #[test]
    fn collect_merges_docs() {
        let id1 = 888881;
        let id2 = 888882;
        set_doc_symbols(id1, &["aaa bbb\n".into()], 100, &HashSet::new());
        set_doc_symbols(id2, &["ccc ddd\n".into()], 100, &HashSet::new());
        let merged = collect(&[id1, id2]);
        assert!(merged.contains(&"aaa".to_string()));
        assert!(merged.contains(&"ccc".to_string()));
        remove_doc(id1);
        remove_doc(id2);
    }
}
