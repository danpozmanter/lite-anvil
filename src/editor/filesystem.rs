use crossbeam_channel::Receiver;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

// ── Directory monitor ────────────────────────────────────────────────────────

/// Core directory monitoring state, independent of Lua.
pub struct DirMonitorInner {
    watcher: RecommendedWatcher,
    rx: Receiver<notify::Result<notify::Event>>,
    path_to_id: HashMap<PathBuf, i32>,
    id_to_path: HashMap<i32, PathBuf>,
    next_id: i32,
}

/// Create a new directory monitor.
pub fn new_dir_monitor(
    on_event: impl Fn() + Send + 'static,
) -> Result<DirMonitorInner, notify::Error> {
    let (tx, rx) = crossbeam_channel::unbounded::<notify::Result<notify::Event>>();
    let watcher = RecommendedWatcher::new(
        move |result| {
            let _ = tx.send(result);
            on_event();
        },
        notify::Config::default(),
    )?;
    Ok(DirMonitorInner {
        watcher,
        rx,
        path_to_id: HashMap::new(),
        id_to_path: HashMap::new(),
        next_id: 0,
    })
}

impl DirMonitorInner {
    /// Watch a path. Returns the watch ID (>= 0), or -1 on error.
    /// Returns the existing ID if already watched.
    pub fn watch(&mut self, path: &str) -> i32 {
        let pb = PathBuf::from(path);
        if let Some(&id) = self.path_to_id.get(&pb) {
            return id;
        }
        match self.watcher.watch(&pb, RecursiveMode::NonRecursive) {
            Ok(()) => {
                let id = self.next_id;
                self.next_id += 1;
                self.path_to_id.insert(pb.clone(), id);
                self.id_to_path.insert(id, pb);
                id
            }
            Err(_) => -1,
        }
    }

    /// Unwatch a previously watched path by ID.
    pub fn unwatch(&mut self, watch_id: i32) {
        if let Some(path) = self.id_to_path.remove(&watch_id) {
            if let Err(e) = self.watcher.unwatch(&path) {
                log::warn!("dirmonitor unwatch failed for {}: {e}", path.display());
            }
            self.path_to_id.remove(&path);
        }
    }

    /// Drain pending events and return the set of fired watch IDs.
    pub fn collect_changes(&self) -> Vec<i32> {
        let mut seen = HashSet::new();
        let mut ids = Vec::new();
        while let Ok(Ok(event)) = self.rx.try_recv() {
            for path in &event.paths {
                let id = self
                    .path_to_id
                    .get(path)
                    .or_else(|| path.parent().and_then(|p| self.path_to_id.get(p)))
                    .copied();
                if let Some(id) = id {
                    if seen.insert(id) {
                        ids.push(id);
                    }
                }
            }
        }
        ids
    }

    /// Monitoring mode identifier.
    pub fn mode(&self) -> &'static str {
        "multiple"
    }
}

// ── Directory walking ────────────────────────────────────────────────────────

/// Options for recursive directory walking.
#[derive(Default)]
pub struct WalkOptions {
    pub show_hidden: bool,
    pub max_size_bytes: Option<u64>,
    pub path_glob: Option<String>,
    pub max_files: Option<usize>,
    pub max_entries: Option<usize>,
    pub exclude_dirs: Vec<String>,
}

/// A single directory entry.
#[derive(Clone, Debug)]
pub struct DirEntry {
    pub name: String,
    pub abs_path: String,
    pub is_dir: bool,
    pub size: u64,
}

/// Returns true if a path component starts with `.`.
pub fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.starts_with('.'))
        .unwrap_or(false)
}

/// Glob pattern matching (supports `*`, `**`, `?`).
pub fn glob_matches(path: &str, glob: &str) -> bool {
    fn inner(path: &[u8], glob: &[u8]) -> bool {
        if glob.is_empty() {
            return path.is_empty();
        }
        if glob.len() >= 2 && glob[0] == b'*' && glob[1] == b'*' {
            if inner(path, &glob[2..]) {
                return true;
            }
            for idx in 0..path.len() {
                if inner(&path[idx + 1..], &glob[2..]) {
                    return true;
                }
            }
            return false;
        }
        match glob[0] {
            b'*' => {
                if inner(path, &glob[1..]) {
                    return true;
                }
                let mut idx = 0usize;
                while idx < path.len() && path[idx] != b'/' {
                    if inner(&path[idx + 1..], &glob[1..]) {
                        return true;
                    }
                    idx += 1;
                }
                false
            }
            b'?' => !path.is_empty() && path[0] != b'/' && inner(&path[1..], &glob[1..]),
            ch => !path.is_empty() && path[0] == ch && inner(&path[1..], &glob[1..]),
        }
    }

    inner(path.as_bytes(), glob.as_bytes())
}

/// Relative path from `root` to `path`, normalized to forward slashes.
pub fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .ok()
        .and_then(|p| p.to_str())
        .unwrap_or("")
        .replace('\\', "/")
}

/// Normalize a root path to forward slashes.
pub fn normalize_root(root: &str) -> String {
    root.replace('\\', "/")
}

/// Sort directory entries: directories before files, natural path ordering.
pub fn sort_entries(entries: &mut [DirEntry]) {
    entries.sort_by(|a, b| {
        let a_type = if a.is_dir { "dir" } else { "file" };
        let b_type = if b.is_dir { "dir" } else { "file" };
        if crate::editor::common::path_compare(&a.name, a_type, &b.name, b_type) {
            std::cmp::Ordering::Less
        } else if crate::editor::common::path_compare(&b.name, b_type, &a.name, a_type) {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    });
}

/// Read and sort entries from a single directory.
pub fn read_dir_entries(
    path: &Path,
    show_hidden: bool,
    max_entries: Option<usize>,
) -> Vec<DirEntry> {
    let mut entries = Vec::new();
    let Ok(read_dir) = fs::read_dir(path) else {
        return entries;
    };
    for entry in read_dir.flatten() {
        let entry_path = entry.path();
        if !show_hidden && is_hidden(&entry_path) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let is_dir = file_type.is_dir();
        let size = if file_type.is_file() {
            entry.metadata().map(|meta| meta.len()).unwrap_or(0)
        } else {
            0
        };
        entries.push(DirEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            abs_path: entry_path.to_string_lossy().into_owned(),
            is_dir,
            size,
        });
        if max_entries.is_some_and(|limit| entries.len() >= limit) {
            break;
        }
    }
    sort_entries(&mut entries);
    entries
}

/// Recursively walk directories and collect file paths.
pub fn walk_files(roots: &[String], opts: &WalkOptions) -> Vec<String> {
    let mut files = Vec::new();
    let mut stack: Vec<(PathBuf, PathBuf)> = roots
        .iter()
        .map(|root| {
            let path = PathBuf::from(root);
            (path.clone(), path)
        })
        .collect();

    while let Some((root, path)) = stack.pop() {
        let entries = read_dir_entries(&path, opts.show_hidden, opts.max_entries);
        for entry in entries {
            let entry_path = PathBuf::from(&entry.abs_path);
            if entry.is_dir {
                if !opts.exclude_dirs.is_empty() {
                    let basename = entry_path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("");
                    if opts.exclude_dirs.iter().any(|d| d == basename) {
                        continue;
                    }
                }
                stack.push((root.clone(), entry_path));
                continue;
            }
            if let Some(limit) = opts.max_size_bytes {
                if entry.size >= limit {
                    continue;
                }
            }
            if let Some(glob) = &opts.path_glob {
                let rel = rel_path(&root, &entry_path);
                if !glob_matches(&rel, glob) {
                    continue;
                }
            }
            files.push(entry.abs_path);
            if opts.max_files.is_some_and(|limit| files.len() >= limit) {
                return files;
            }
        }
    }
    files
}

// ── Gitignore pattern conversion ─────────────────────────────────────────────

/// Convert a gitignore glob pattern to a Lua-compatible pattern string.
pub fn glob_to_lua_pattern(glob: &str) -> String {
    let bytes = glob.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'*' {
            out.push_str(".*");
            i += 2;
        } else if bytes[i] == b'*' {
            out.push_str("[^/]*");
            i += 1;
        } else if bytes[i] == b'?' {
            out.push_str("[^/]");
            i += 1;
        } else {
            let ch = bytes[i] as char;
            if "%+-^$().[]?".contains(ch) {
                out.push('%');
            }
            out.push(ch);
            i += 1;
        }
    }
    out
}

/// Parsed gitignore rule (pure data, no Lua types).
#[derive(Debug, Clone)]
pub struct GitignoreRule {
    pub base_dir: Option<String>,
    pub negated: bool,
    pub dir_only: bool,
    pub anchored: bool,
    pub has_slash: bool,
    pub pattern: String,
    pub raw: String,
}

/// Parse a single gitignore line into a rule. Returns `None` for empty/comment lines.
pub fn parse_gitignore_rule(line: &str, base_dir: &str) -> Option<GitignoreRule> {
    if line.is_empty() || line.trim_start().starts_with('#') {
        return None;
    }

    let mut s = line;
    let negated = s.starts_with('!');
    if negated {
        s = &s[1..];
    }

    let anchored = s.starts_with('/');
    if anchored {
        s = &s[1..];
    }

    let dir_only = s.ends_with('/');
    if dir_only {
        s = &s[..s.len() - 1];
    }

    if s.is_empty() {
        return None;
    }

    let has_slash = s.contains('/');
    let prefix = if anchored {
        "^"
    } else if has_slash {
        "^(.-/)?"
    } else {
        "^([^/]+/)*"
    };

    let mut pattern = String::from(prefix);
    pattern.push_str(&glob_to_lua_pattern(s));
    if dir_only {
        pattern.push_str("(/.*)?$");
    } else {
        pattern.push('$');
    }

    Some(GitignoreRule {
        base_dir: Some(base_dir.to_string()),
        negated,
        dir_only,
        anchored,
        has_slash,
        pattern,
        raw: s.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_hidden_dot_file() {
        assert!(is_hidden(Path::new("/foo/.gitignore")));
        assert!(!is_hidden(Path::new("/foo/bar.txt")));
    }

    #[test]
    fn glob_matches_star() {
        assert!(glob_matches("foo.rs", "*.rs"));
        assert!(!glob_matches("foo.txt", "*.rs"));
    }

    #[test]
    fn glob_matches_double_star() {
        assert!(glob_matches("src/main.rs", "**/*.rs"));
        assert!(glob_matches("deep/nested/file.rs", "**/*.rs"));
    }

    #[test]
    fn glob_matches_question() {
        assert!(glob_matches("a.rs", "?.rs"));
        assert!(!glob_matches("ab.rs", "?.rs"));
    }

    #[test]
    fn rel_path_basic() {
        let root = Path::new("/project");
        let file = Path::new("/project/src/main.rs");
        assert_eq!(rel_path(root, file), "src/main.rs");
    }

    #[test]
    fn glob_to_lua_pattern_star() {
        assert_eq!(glob_to_lua_pattern("*.rs"), "[^/]*%.rs");
    }

    #[test]
    fn glob_to_lua_pattern_double_star() {
        assert_eq!(glob_to_lua_pattern("**/*.rs"), ".*/[^/]*%.rs");
    }

    #[test]
    fn glob_to_lua_pattern_question() {
        assert_eq!(glob_to_lua_pattern("?.txt"), "[^/]%.txt");
    }

    #[test]
    fn parse_gitignore_rule_simple() {
        let rule = parse_gitignore_rule("*.o", "/project").unwrap();
        assert!(!rule.negated);
        assert!(!rule.dir_only);
        assert!(!rule.anchored);
        assert_eq!(rule.raw, "*.o");
        assert!(rule.pattern.contains("[^/]*%.o"));
    }

    #[test]
    fn parse_gitignore_rule_negated() {
        let rule = parse_gitignore_rule("!important.o", "/project").unwrap();
        assert!(rule.negated);
    }

    #[test]
    fn parse_gitignore_rule_dir_only() {
        let rule = parse_gitignore_rule("build/", "/project").unwrap();
        assert!(rule.dir_only);
        assert!(rule.pattern.ends_with("(/.*)?$"));
    }

    #[test]
    fn parse_gitignore_rule_comment() {
        assert!(parse_gitignore_rule("# comment", "/project").is_none());
    }

    #[test]
    fn parse_gitignore_rule_empty() {
        assert!(parse_gitignore_rule("", "/project").is_none());
    }

    #[test]
    fn dir_monitor_watch_unwatch() {
        let monitor = new_dir_monitor(|| {});
        assert!(monitor.is_ok());
        let mut monitor = monitor.unwrap();
        let tmp = std::env::temp_dir();
        let id = monitor.watch(tmp.to_str().unwrap());
        assert!(id >= 0);
        // Watch same path returns same ID
        let id2 = monitor.watch(tmp.to_str().unwrap());
        assert_eq!(id, id2);
        monitor.unwatch(id);
        // After unwatch, re-watching gives a new ID
        let id3 = monitor.watch(tmp.to_str().unwrap());
        assert_ne!(id, id3);
    }

    #[test]
    fn collect_changes_empty() {
        let monitor = new_dir_monitor(|| {}).unwrap();
        assert!(monitor.collect_changes().is_empty());
    }
}
