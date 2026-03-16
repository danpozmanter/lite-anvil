use mlua::prelude::*;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use pcre2::bytes::Regex;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

const REBUILD_DEBOUNCE: Duration = Duration::from_millis(250);

#[cfg(feature = "sdl")]
const WAKEUP_RATE_LIMIT: Duration = Duration::from_millis(200);

static GLOBAL_GENERATION: AtomicU64 = AtomicU64::new(1);

struct TreeEntry {
    snapshot: Arc<Mutex<Option<ProjectSnapshot>>>,
    dirty: Arc<AtomicBool>,
    rebuilding: Arc<AtomicBool>,
    last_event: Arc<Mutex<Option<Instant>>>,
    _watcher: Arc<Mutex<Option<RecommendedWatcher>>>,
    expanded: Arc<Mutex<HashSet<String>>>,
    options: TreeOptionsKey,
}

static TREES: Lazy<Mutex<HashMap<String, TreeEntry>>> = Lazy::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, PartialEq, Eq)]
struct TreeOptionsKey {
    show_hidden: bool,
    show_ignored: bool,
    max_entries: Option<usize>,
    file_size_limit_bytes: Option<u64>,
    ignore_files: Vec<String>,
    gitignore_enabled: bool,
    gitignore_additional_patterns: Vec<String>,
}

#[derive(Clone)]
struct TreeOptions {
    key: TreeOptionsKey,
    ignore_files: Vec<CompiledIgnoreRule>,
    gitignore_additional_patterns: Vec<CompiledLuaPattern>,
}

#[derive(Clone)]
struct CompiledIgnoreRule {
    use_path: bool,
    match_dir: bool,
    regex: Regex,
}

#[derive(Clone)]
struct CompiledLuaPattern {
    regex: Regex,
}

#[derive(Clone)]
struct GitignoreRule {
    dir_only: bool,
    negated: bool,
    has_slash: bool,
    anchored: bool,
    regex: Regex,
}

#[derive(Clone)]
struct ProjectSnapshot {
    nodes: Vec<TreeNode>,
    visible: Vec<usize>,
    visible_index: HashMap<String, usize>,
}

#[derive(Clone)]
struct TreeNode {
    name: String,
    abs_path: String,
    kind: NodeKind,
    ignored: bool,
    depth: usize,
    children: Vec<usize>,
    expanded: bool,
    project_root: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NodeKind {
    Dir,
    File,
}

impl NodeKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Dir => "dir",
            Self::File => "file",
        }
    }
}

#[derive(Clone)]
struct DirEntry {
    name: String,
    abs_path: String,
    kind: NodeKind,
    size: u64,
}

fn bump_generation() {
    GLOBAL_GENERATION.fetch_add(1, Ordering::Relaxed);
    #[cfg(feature = "sdl")]
    crate::window::push_wakeup_event();
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
}

fn path_belongs_to(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.to_string())
}

fn dirname(path: &str) -> Option<String> {
    Path::new(path)
        .parent()
        .and_then(|parent| parent.to_str())
        .map(normalize_path)
}

fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.starts_with('.'))
        .unwrap_or(false)
}

fn read_dir_entries(path: &Path, show_hidden: bool, max_entries: Option<usize>) -> Vec<DirEntry> {
    let mut entries = Vec::new();
    if let Ok(read_dir) = fs::read_dir(path) {
        for entry in read_dir.flatten() {
            let entry_path = entry.path();
            if !show_hidden && is_hidden(&entry_path) {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let kind = if file_type.is_dir() {
                NodeKind::Dir
            } else {
                NodeKind::File
            };
            let size = if kind == NodeKind::File {
                entry.metadata().map(|meta| meta.len()).unwrap_or(0)
            } else {
                0
            };
            entries.push(DirEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                abs_path: normalize_path(&entry_path.to_string_lossy()),
                kind,
                size,
            });
            if max_entries.is_some_and(|limit| entries.len() >= limit) {
                break;
            }
        }
    }
    entries.sort_by(|a, b| {
        if super::path_compare(&a.name, a.kind.as_str(), &b.name, b.kind.as_str()) {
            std::cmp::Ordering::Less
        } else if super::path_compare(&b.name, b.kind.as_str(), &a.name, a.kind.as_str()) {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    });
    entries
}

fn lua_class_to_regex(ch: char) -> Option<&'static str> {
    match ch {
        'a' => Some("A-Za-z"),
        'A' => Some("^A-Za-z"),
        'd' => Some("0-9"),
        'D' => Some("^0-9"),
        'l' => Some("a-z"),
        'L' => Some("^a-z"),
        'u' => Some("A-Z"),
        'U' => Some("^A-Z"),
        'w' => Some("A-Za-z0-9"),
        'W' => Some("^A-Za-z0-9"),
        's' => Some("\\s"),
        'S' => Some("\\S"),
        'p' => Some(r#"!-/:-@\[-`{-~"#),
        'P' => Some(r#"^!-/:-@\[-`{-~"#),
        'x' => Some("A-Fa-f0-9"),
        'X' => Some("^A-Fa-f0-9"),
        _ => None,
    }
}

fn escape_regex_char(ch: char) -> String {
    match ch {
        '.' | '\\' | '+' | '*' | '?' | '^' | '$' | '(' | ')' | '[' | ']' | '{' | '}' | '|' => {
            format!("\\{ch}")
        }
        _ => ch.to_string(),
    }
}

fn parse_class(chars: &[char], start: usize) -> (String, usize) {
    let mut out = String::from("[");
    let mut idx = start + 1;
    if idx < chars.len() && chars[idx] == '^' {
        out.push('^');
        idx += 1;
    }
    while idx < chars.len() {
        let ch = chars[idx];
        if ch == ']' {
            out.push(']');
            return (out, idx + 1);
        }
        if ch == '%' && idx + 1 < chars.len() {
            let cls = chars[idx + 1];
            if let Some(mapped) = lua_class_to_regex(cls) {
                out.push_str(mapped);
            } else {
                out.push_str(&escape_regex_char(cls));
            }
            idx += 2;
            continue;
        }
        if matches!(ch, '\\' | ']' | '[' | '^') {
            out.push('\\');
        }
        out.push(ch);
        idx += 1;
    }
    ("\\[".to_string(), start + 1)
}

fn lua_pattern_to_regex(pattern: &str) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut idx = 0usize;
    while idx < chars.len() {
        let ch = chars[idx];
        match ch {
            '%' if idx + 1 < chars.len() => {
                let next = chars[idx + 1];
                match next {
                    'b' => {
                        out.push_str(r"\b");
                    }
                    'f' => {
                        out.push_str("");
                    }
                    other => {
                        if let Some(mapped) = lua_class_to_regex(other) {
                            if mapped.starts_with('^') {
                                out.push('[');
                                out.push_str(mapped);
                                out.push(']');
                            } else if mapped == "\\s" || mapped == "\\S" {
                                out.push_str(mapped);
                            } else {
                                out.push('[');
                                out.push_str(mapped);
                                out.push(']');
                            }
                        } else {
                            out.push_str(&escape_regex_char(other));
                        }
                    }
                }
                idx += 2;
            }
            '[' => {
                let (class, next) = parse_class(&chars, idx);
                out.push_str(&class);
                idx = next;
            }
            '.' => {
                out.push('.');
                idx += 1;
            }
            '*' => {
                out.push('*');
                idx += 1;
            }
            '+' => {
                out.push('+');
                idx += 1;
            }
            '-' => {
                out.push_str("*?");
                idx += 1;
            }
            '?' => {
                out.push('?');
                idx += 1;
            }
            '^' | '$' => {
                out.push(ch);
                idx += 1;
            }
            _ => {
                out.push_str(&escape_regex_char(ch));
                idx += 1;
            }
        }
    }
    out
}

fn compile_lua_pattern(pattern: &str) -> Option<CompiledLuaPattern> {
    Regex::new(&lua_pattern_to_regex(pattern))
        .ok()
        .map(|regex| CompiledLuaPattern { regex })
}

fn ignore_rule_uses_path(pattern: &str) -> bool {
    match pattern.find('/') {
        Some(idx) => idx + 1 < pattern.len() && !pattern.ends_with('/') && !pattern.ends_with("/$"),
        None => false,
    }
}

fn ignore_rule_matches_dir(pattern: &str) -> bool {
    pattern.ends_with('/') || pattern.ends_with("/$")
}

fn compile_ignore_rule(pattern: &str) -> Option<CompiledIgnoreRule> {
    Regex::new(&lua_pattern_to_regex(pattern))
        .ok()
        .map(|regex| CompiledIgnoreRule {
            use_path: ignore_rule_uses_path(pattern),
            match_dir: ignore_rule_matches_dir(pattern),
            regex,
        })
}

fn glob_to_regex(glob: &str) -> String {
    let chars: Vec<char> = glob.chars().collect();
    let mut out = String::new();
    let mut idx = 0usize;
    while idx < chars.len() {
        if idx + 1 < chars.len() && chars[idx] == '*' && chars[idx + 1] == '*' {
            out.push_str(".*");
            idx += 2;
            continue;
        }
        match chars[idx] {
            '*' => out.push_str("[^/]*"),
            '?' => out.push_str("[^/]"),
            ch => out.push_str(&escape_regex_char(ch)),
        }
        idx += 1;
    }
    out
}

fn parse_gitignore_rule(line: &str, _base_dir: &str) -> Option<GitignoreRule> {
    if line.is_empty() || line.trim_start().starts_with('#') {
        return None;
    }
    let mut line = line.to_string();
    let mut negated = false;
    if line.starts_with('!') {
        negated = true;
        line.remove(0);
    }
    let anchored = line.starts_with('/');
    if anchored {
        line.remove(0);
    }
    let dir_only = line.ends_with('/');
    if dir_only {
        line.pop();
    }
    if line.is_empty() {
        return None;
    }
    let has_slash = line.contains('/');
    let prefix = if anchored {
        "^".to_string()
    } else if has_slash {
        "^(.-/)?".to_string()
    } else {
        "^([^/]+/)*".to_string()
    };
    let mut pattern = prefix;
    pattern.push_str(&glob_to_regex(&line));
    if dir_only {
        pattern.push_str("(/.*)?$");
    } else {
        pattern.push('$');
    }
    Regex::new(&pattern).ok().map(|regex| GitignoreRule {
        dir_only,
        negated,
        has_slash,
        anchored,
        regex,
    })
}

fn find_git_root(start_path: &str) -> Option<String> {
    let mut current = normalize_path(start_path);
    loop {
        let git_dir = Path::new(&current).join(".git");
        if git_dir.exists() {
            return Some(current);
        }
        let parent = dirname(&current)?;
        if parent == current {
            return None;
        }
        current = parent;
    }
}

fn relative_to(root: &str, path: &str) -> Option<String> {
    Path::new(path)
        .strip_prefix(Path::new(root))
        .ok()
        .and_then(|rel| rel.to_str())
        .map(normalize_path)
}

fn compile_options(key: TreeOptionsKey) -> TreeOptions {
    TreeOptions {
        ignore_files: key
            .ignore_files
            .iter()
            .filter_map(|pattern| compile_ignore_rule(pattern))
            .collect(),
        gitignore_additional_patterns: key
            .gitignore_additional_patterns
            .iter()
            .filter_map(|pattern| compile_lua_pattern(pattern))
            .collect(),
        key,
    }
}

fn matches_compiled_lua(pattern: &CompiledLuaPattern, text: &str) -> bool {
    pattern.regex.is_match(text.as_bytes()).unwrap_or(false)
}

fn matches_ignore_rule(rule: &CompiledIgnoreRule, basename: &str, fullname: &str, kind: NodeKind) -> bool {
    let test = if rule.use_path { fullname } else { basename };
    if rule.match_dir {
        kind == NodeKind::Dir && rule.regex.is_match(format!("{test}/").as_bytes()).unwrap_or(false)
    } else {
        rule.regex.is_match(test.as_bytes()).unwrap_or(false)
    }
}

fn load_gitignore_rules(
    dir: &str,
    cache: &mut HashMap<String, Vec<GitignoreRule>>,
) -> Vec<GitignoreRule> {
    if let Some(existing) = cache.get(dir) {
        return existing.clone();
    }
    let mut rules = Vec::new();
    let ignore_path = Path::new(dir).join(".gitignore");
    if let Ok(contents) = fs::read_to_string(ignore_path) {
        for line in contents.lines() {
            if let Some(rule) = parse_gitignore_rule(line, dir) {
                rules.push(rule);
            }
        }
    }
    cache.insert(dir.to_string(), rules.clone());
    rules
}

fn collect_gitignore_dirs(root: &str, path: &str, kind: NodeKind) -> Vec<String> {
    let mut dirs = Vec::new();
    let mut current = if kind == NodeKind::File {
        dirname(path)
    } else {
        Some(path.to_string())
    };
    while let Some(dir) = current {
        if !path_belongs_to(&dir, root) {
            break;
        }
        dirs.push(dir.clone());
        if dir == root {
            break;
        }
        current = dirname(&dir);
    }
    dirs.reverse();
    dirs
}

fn is_ignored(
    opts: &TreeOptions,
    git_root: &str,
    git_cache: &mut HashMap<String, Vec<GitignoreRule>>,
    path: &str,
    kind: NodeKind,
    size: u64,
) -> bool {
    if opts
        .key
        .file_size_limit_bytes
        .is_some_and(|limit| size >= limit)
    {
        return true;
    }

    let basename = basename(path);
    let fullname = format!("/{}", normalize_path(path));
    for rule in &opts.ignore_files {
        if matches_ignore_rule(rule, &basename, &fullname, kind) {
            return true;
        }
    }

    if opts.key.gitignore_enabled && path_belongs_to(path, git_root) {
        let mut ignored = false;
        for dir in collect_gitignore_dirs(git_root, path, kind) {
            let rel = relative_to(&dir, path).unwrap_or_default();
            for rule in load_gitignore_rules(&dir, git_cache) {
                let target = if !rule.has_slash && !rule.anchored {
                    basename.clone()
                } else {
                    rel.clone()
                };
                if rule.regex.is_match(target.as_bytes()).unwrap_or(false)
                    && (!rule.dir_only || kind == NodeKind::Dir)
                {
                    ignored = !rule.negated;
                }
            }
        }
        if ignored {
            return true;
        }
    }

    if !opts.gitignore_additional_patterns.is_empty() {
        let rel = relative_to(git_root, path).unwrap_or_else(|| basename.clone());
        if opts
            .gitignore_additional_patterns
            .iter()
            .any(|pattern| {
                matches_compiled_lua(pattern, &rel) || matches_compiled_lua(pattern, &basename)
            })
        {
            return true;
        }
    }

    false
}

fn rebuild_visible(snapshot: &mut ProjectSnapshot) {
    snapshot.visible.clear();
    snapshot.visible_index.clear();
    let mut stack = vec![0usize];
    while let Some(node_id) = stack.pop() {
        let idx = snapshot.visible.len();
        let path = snapshot.nodes[node_id].abs_path.clone();
        snapshot.visible.push(node_id);
        snapshot.visible_index.insert(path, idx + 1);
        if snapshot.nodes[node_id].kind == NodeKind::Dir && snapshot.nodes[node_id].expanded {
            for child in snapshot.nodes[node_id].children.iter().rev() {
                stack.push(*child);
            }
        }
    }
}

fn build_snapshot(root: &str, opts: &TreeOptions, expanded: &HashSet<String>) -> ProjectSnapshot {
    let root = normalize_path(root);
    let git_root = find_git_root(&root).unwrap_or_else(|| root.clone());
    let mut git_cache = HashMap::new();
    let mut nodes = vec![TreeNode {
        name: basename(&root),
        abs_path: root.clone(),
        kind: NodeKind::Dir,
        ignored: false,
        depth: 0,
        children: Vec::new(),
        expanded: true,
        project_root: root.clone(),
    }];
    let mut stack = vec![(0usize, PathBuf::from(&root))];

    while let Some((parent_id, path)) = stack.pop() {
        let depth = nodes[parent_id].depth + 1;
        let entries = read_dir_entries(&path, opts.key.show_hidden, opts.key.max_entries);
        let mut children = Vec::new();
        for entry in entries.into_iter().rev() {
            let ignored = is_ignored(
                opts,
                &git_root,
                &mut git_cache,
                &entry.abs_path,
                entry.kind,
                entry.size,
            );
            if ignored && !opts.key.show_ignored {
                continue;
            }
            let child_id = nodes.len();
            let child_path = entry.abs_path.clone();
            let is_expanded = entry.kind == NodeKind::Dir
                && (child_path == root || expanded.contains(&child_path));
            nodes.push(TreeNode {
                name: entry.name,
                abs_path: child_path.clone(),
                kind: entry.kind,
                ignored,
                depth,
                children: Vec::new(),
                expanded: is_expanded,
                project_root: root.clone(),
            });
            children.push(child_id);
            if entry.kind == NodeKind::Dir {
                stack.push((child_id, PathBuf::from(&child_path)));
            }
        }
        children.reverse();
        nodes[parent_id].children = children;
    }

    let mut snapshot = ProjectSnapshot {
        nodes,
        visible: Vec::new(),
        visible_index: HashMap::new(),
    };
    rebuild_visible(&mut snapshot);
    snapshot
}

fn parse_opts(opts: Option<LuaTable>) -> LuaResult<TreeOptionsKey> {
    let mut key = TreeOptionsKey {
        show_hidden: false,
        show_ignored: true,
        max_entries: None,
        file_size_limit_bytes: None,
        ignore_files: Vec::new(),
        gitignore_enabled: true,
        gitignore_additional_patterns: Vec::new(),
    };
    if let Some(opts) = opts {
        key.show_hidden = opts.get::<Option<bool>>("show_hidden")?.unwrap_or(false);
        key.show_ignored = opts.get::<Option<bool>>("show_ignored")?.unwrap_or(true);
        key.max_entries = opts.get::<Option<usize>>("max_entries")?;
        key.file_size_limit_bytes = opts.get::<Option<u64>>("file_size_limit_bytes")?;
        key.gitignore_enabled = opts
            .get::<Option<bool>>("gitignore_enabled")?
            .unwrap_or(true);
        if let Some(table) = opts.get::<Option<LuaTable>>("ignore_files")? {
            for value in table.sequence_values::<String>() {
                key.ignore_files.push(value?);
            }
        }
        if let Some(table) = opts.get::<Option<LuaTable>>("gitignore_additional_patterns")? {
            for value in table.sequence_values::<String>() {
                key.gitignore_additional_patterns.push(value?);
            }
        }
    }
    Ok(key)
}

fn ensure_tree(root: &str, options: &TreeOptionsKey) -> LuaResult<()> {
    let root = normalize_path(root);
    enum Work {
        None,
        Rebuild {
            snapshot: Arc<Mutex<Option<ProjectSnapshot>>>,
            rebuilding: Arc<AtomicBool>,
            expanded: Arc<Mutex<HashSet<String>>>,
            options: TreeOptionsKey,
        },
        New {
            snapshot: Arc<Mutex<Option<ProjectSnapshot>>>,
            dirty: Arc<AtomicBool>,
            rebuilding: Arc<AtomicBool>,
            last_event: Arc<Mutex<Option<Instant>>>,
            watcher: Arc<Mutex<Option<RecommendedWatcher>>>,
            expanded: Arc<Mutex<HashSet<String>>>,
            options: TreeOptionsKey,
        },
    }

    let work = {
        let mut trees = TREES.lock();
        match trees.get_mut(&root) {
            Some(entry) => {
                let config_changed = entry.options != *options;
                let is_dirty = entry.dirty.load(Ordering::Relaxed);
                if entry.rebuilding.load(Ordering::Relaxed) {
                    Work::None
                } else if !config_changed && !is_dirty {
                    Work::None
                } else {
                    if is_dirty && !config_changed {
                        let elapsed = entry
                            .last_event
                            .lock()
                            .as_ref()
                            .map(|t| t.elapsed())
                            .unwrap_or(Duration::MAX);
                        if elapsed < REBUILD_DEBOUNCE {
                            return Ok(());
                        }
                    }
                    entry.dirty.store(false, Ordering::Relaxed);
                    entry.rebuilding.store(true, Ordering::Relaxed);
                    entry.options = options.clone();
                    Work::Rebuild {
                        snapshot: Arc::clone(&entry.snapshot),
                        rebuilding: Arc::clone(&entry.rebuilding),
                        expanded: Arc::clone(&entry.expanded),
                        options: options.clone(),
                    }
                }
            }
            None => {
                let snapshot = Arc::new(Mutex::new(None::<ProjectSnapshot>));
                let dirty = Arc::new(AtomicBool::new(false));
                let rebuilding = Arc::new(AtomicBool::new(true));
                let last_event = Arc::new(Mutex::new(None::<Instant>));
                let watcher = Arc::new(Mutex::new(None::<RecommendedWatcher>));
                let mut expanded = HashSet::new();
                expanded.insert(root.clone());
                let expanded = Arc::new(Mutex::new(expanded));
                trees.insert(
                    root.clone(),
                    TreeEntry {
                        snapshot: Arc::clone(&snapshot),
                        dirty: Arc::clone(&dirty),
                        rebuilding: Arc::clone(&rebuilding),
                        last_event: Arc::clone(&last_event),
                        _watcher: Arc::clone(&watcher),
                        expanded: Arc::clone(&expanded),
                        options: options.clone(),
                    },
                );
                Work::New {
                    snapshot,
                    dirty,
                    rebuilding,
                    last_event,
                    watcher,
                    expanded,
                    options: options.clone(),
                }
            }
        }
    };

    match work {
        Work::None => {}
        Work::Rebuild {
            snapshot,
            rebuilding,
            expanded,
            options,
        } => {
            let root_clone = root.clone();
            std::thread::spawn(move || {
                let opts = compile_options(options);
                let expanded_set = expanded.lock().clone();
                let new_snapshot = build_snapshot(&root_clone, &opts, &expanded_set);
                *snapshot.lock() = Some(new_snapshot);
                rebuilding.store(false, Ordering::Relaxed);
                bump_generation();
            });
        }
        Work::New {
            snapshot,
            dirty,
            rebuilding,
            last_event,
            watcher,
            expanded,
            options,
        } => {
            let dirty_for_cb = Arc::clone(&dirty);
            let last_event_for_cb = Arc::clone(&last_event);
            #[cfg(feature = "sdl")]
            let last_wakeup_for_cb: Arc<Mutex<Option<Instant>>> =
                Arc::new(Mutex::new(None::<Instant>));
            let root_clone = root.clone();
            std::thread::spawn(move || {
                let opts = compile_options(options.clone());
                let expanded_set = expanded.lock().clone();
                let new_snapshot = build_snapshot(&root_clone, &opts, &expanded_set);
                *snapshot.lock() = Some(new_snapshot);
                rebuilding.store(false, Ordering::Relaxed);
                bump_generation();

                let watcher_result = (|| -> Result<RecommendedWatcher, notify::Error> {
                    let mut w = RecommendedWatcher::new(
                        move |_res: notify::Result<notify::Event>| {
                            dirty_for_cb.store(true, Ordering::Relaxed);
                            *last_event_for_cb.lock() = Some(Instant::now());
                            #[cfg(feature = "sdl")]
                            {
                                let should_push = {
                                    let mut guard = last_wakeup_for_cb.lock();
                                    let now = Instant::now();
                                    let elapsed = guard
                                        .as_ref()
                                        .map(|t| now.duration_since(*t))
                                        .unwrap_or(Duration::MAX);
                                    if elapsed >= WAKEUP_RATE_LIMIT {
                                        *guard = Some(now);
                                        true
                                    } else {
                                        false
                                    }
                                };
                                if should_push {
                                    crate::window::push_wakeup_event();
                                }
                            }
                        },
                        notify::Config::default(),
                    )?;
                    w.watch(Path::new(&root_clone), RecursiveMode::Recursive)?;
                    Ok(w)
                })();
                if let Ok(native_watcher) = watcher_result {
                    *watcher.lock() = Some(native_watcher);
                }
            });
        }
    }

    Ok(())
}

fn prune_roots(roots: &[String]) {
    let keep: HashSet<String> = roots.iter().cloned().collect();
    TREES.lock().retain(|root, _| keep.contains(root));
}

fn collect_snapshots(roots: &[String]) -> Vec<ProjectSnapshot> {
    let trees = TREES.lock();
    roots.iter()
        .filter_map(|root| trees.get(root))
        .filter_map(|entry| entry.snapshot.lock().clone())
        .collect()
}

fn item_to_lua(lua: &Lua, node: &TreeNode) -> LuaResult<LuaTable> {
    let table = lua.create_table()?;
    table.set("name", node.name.as_str())?;
    table.set("abs_filename", node.abs_path.as_str())?;
    table.set("type", node.kind.as_str())?;
    table.set("depth", node.depth as i64)?;
    table.set("ignored", node.ignored)?;
    table.set("expanded", node.expanded)?;
    table.set("project_root", node.project_root.as_str())?;
    Ok(table)
}

fn find_root_for_path(path: &str) -> Option<String> {
    let path = normalize_path(path);
    let trees = TREES.lock();
    trees.keys()
        .filter(|root| path_belongs_to(&path, root))
        .max_by_key(|root| root.len())
        .cloned()
}

fn update_visible_for_root(root: &str) -> bool {
    let trees = TREES.lock();
    let Some(entry) = trees.get(root) else {
        return false;
    };
    let expanded = entry.expanded.lock().clone();
    let mut snapshot_guard = entry.snapshot.lock();
    let Some(snapshot) = snapshot_guard.as_mut() else {
        return false;
    };
    let mut changed = false;
    for node in &mut snapshot.nodes {
        if node.kind == NodeKind::Dir {
            let next = node.abs_path == root || expanded.contains(&node.abs_path);
            if node.expanded != next {
                node.expanded = next;
                changed = true;
            }
        }
    }
    if changed {
        rebuild_visible(snapshot);
    }
    changed
}

pub fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    let module = lua.create_table()?;

    module.set(
        "sync_roots",
        lua.create_function(|_, (roots, opts): (LuaTable, Option<LuaTable>)| {
            let opts = parse_opts(opts)?;
            let mut keep = Vec::new();
            for root in roots.sequence_values::<String>() {
                let root = normalize_path(&root?);
                ensure_tree(&root, &opts)?;
                keep.push(root);
            }
            prune_roots(&keep);
            Ok(true)
        })?,
    )?;

    module.set(
        "generation",
        lua.create_function(|_, ()| Ok(GLOBAL_GENERATION.load(Ordering::Relaxed)))?,
    )?;

    module.set(
        "get_visible_items",
        lua.create_function(|lua, (roots, opts): (LuaTable, Option<LuaTable>)| {
            let opts = parse_opts(opts)?;
            let mut root_list = Vec::new();
            for root in roots.sequence_values::<String>() {
                let root = normalize_path(&root?);
                ensure_tree(&root, &opts)?;
                root_list.push(root);
            }
            let snapshots = collect_snapshots(&root_list);
            let total: usize = snapshots.iter().map(|snapshot| snapshot.visible.len()).sum();
            let out = lua.create_table_with_capacity(total, 0)?;
            let mut idx = 1i64;
            for snapshot in snapshots {
                for node_id in snapshot.visible {
                    out.raw_set(idx, item_to_lua(lua, &snapshot.nodes[node_id])?)?;
                    idx += 1;
                }
            }
            Ok(out)
        })?,
    )?;

    module.set(
        "toggle_expand",
        lua.create_function(|_, (path, toggle): (String, Option<bool>)| {
            let path = normalize_path(&path);
            let Some(root) = find_root_for_path(&path) else {
                return Ok(false);
            };
            let trees = TREES.lock();
            let Some(entry) = trees.get(&root) else {
                return Ok(false);
            };
            let mut expanded = entry.expanded.lock();
            let is_expanded = expanded.contains(&path) || path == root;
            let next = toggle.unwrap_or(!is_expanded);
            if path != root {
                if next {
                    expanded.insert(path.clone());
                } else {
                    expanded.remove(&path);
                }
            }
            drop(expanded);
            drop(trees);
            if update_visible_for_root(&root) {
                bump_generation();
            }
            Ok(true)
        })?,
    )?;

    module.set(
        "expand_to",
        lua.create_function(|_, path: String| {
            let path = normalize_path(&path);
            let Some(root) = find_root_for_path(&path) else {
                return Ok(false);
            };
            let trees = TREES.lock();
            let Some(entry) = trees.get(&root) else {
                return Ok(false);
            };
            let mut expanded = entry.expanded.lock();
            let mut current = dirname(&path);
            while let Some(dir) = current {
                if !path_belongs_to(&dir, &root) {
                    break;
                }
                expanded.insert(dir.clone());
                if dir == root {
                    break;
                }
                current = dirname(&dir);
            }
            drop(expanded);
            drop(trees);
            if update_visible_for_root(&root) {
                bump_generation();
            }
            Ok(true)
        })?,
    )?;

    module.set(
        "get_row",
        lua.create_function(|_, path: String| {
            let path = normalize_path(&path);
            let Some(root) = find_root_for_path(&path) else {
                return Ok(None::<i64>);
            };
            let trees = TREES.lock();
            let Some(entry) = trees.get(&root) else {
                return Ok(None::<i64>);
            };
            let snapshot_guard = entry.snapshot.lock();
            let Some(snapshot) = snapshot_guard.as_ref() else {
                return Ok(None::<i64>);
            };
            Ok(snapshot.visible_index.get(&path).copied().map(|idx| idx as i64))
        })?,
    )?;

    module.set(
        "invalidate",
        lua.create_function(|_, root: String| {
            let root = normalize_path(&root);
            let trees = TREES.lock();
            let Some(entry) = trees.get(&root) else {
                return Ok(false);
            };
            entry.dirty.store(true, Ordering::Relaxed);
            *entry.last_event.lock() = Some(Instant::now() - REBUILD_DEBOUNCE);
            Ok(true)
        })?,
    )?;

    module.set(
        "clear_all",
        lua.create_function(|_, ()| {
            TREES.lock().clear();
            bump_generation();
            Ok(true)
        })?,
    )?;

    Ok(module)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempTree {
        path: PathBuf,
    }

    impl TempTree {
        fn new() -> Self {
            let unique = format!(
                "lite-anvil-tree-model-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            );
            let path = std::env::temp_dir().join(unique);
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn path_str(&self) -> String {
            self.path.to_string_lossy().into_owned()
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn write_file(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    fn base_options() -> TreeOptions {
        compile_options(TreeOptionsKey {
            show_hidden: false,
            show_ignored: true,
            max_entries: None,
            file_size_limit_bytes: None,
            ignore_files: vec!["^node_modules/".to_string(), "%.pyc$".to_string()],
            gitignore_enabled: true,
            gitignore_additional_patterns: vec!["^dist/".to_string()],
        })
    }

    #[test]
    fn snapshot_respects_expansion_state() {
        let tree = TempTree::new();
        write_file(&tree.path.join("src").join("main.rs"), b"fn main() {}");
        write_file(&tree.path.join("README.md"), b"hi");

        let mut expanded = HashSet::new();
        expanded.insert(tree.path_str());
        let collapsed = build_snapshot(&tree.path_str(), &base_options(), &expanded);
        let collapsed_names: Vec<_> = collapsed
            .visible
            .iter()
            .map(|id| collapsed.nodes[*id].name.clone())
            .collect();
        assert_eq!(
            collapsed_names,
            vec![
                basename(&tree.path_str()),
                "src".to_string(),
                "README.md".to_string(),
            ]
        );

        expanded.insert(normalize_path(&tree.path.join("src").to_string_lossy()));
        let expanded_snapshot = build_snapshot(&tree.path_str(), &base_options(), &expanded);
        let expanded_names: Vec<_> = expanded_snapshot
            .visible
            .iter()
            .map(|id| expanded_snapshot.nodes[*id].name.clone())
            .collect();
        assert_eq!(
            expanded_names,
            vec![
                basename(&tree.path_str()),
                "src".to_string(),
                "main.rs".to_string(),
                "README.md".to_string(),
            ]
        );
    }

    #[test]
    fn snapshot_applies_ignore_and_gitignore_rules() {
        let tree = TempTree::new();
        write_file(&tree.path.join(".gitignore"), b"target/\nignored.txt\n");
        write_file(&tree.path.join("kept.txt"), b"ok");
        write_file(&tree.path.join("ignored.txt"), b"no");
        write_file(&tree.path.join("target").join("build.log"), b"log");
        write_file(&tree.path.join("dist").join("bundle.js"), b"bundle");
        write_file(&tree.path.join("module.pyc"), b"pyc");

        let expanded = HashSet::from([tree.path_str()]);
        let snapshot = build_snapshot(&tree.path_str(), &base_options(), &expanded);
        let names: Vec<_> = snapshot
            .visible
            .iter()
            .map(|id| snapshot.nodes[*id].name.as_str())
            .collect();

        assert!(names.contains(&"kept.txt"));
        assert!(names.contains(&"ignored.txt"));
        assert!(names.contains(&"target"));
        assert!(names.contains(&"dist"));
        assert!(names.contains(&"module.pyc"));

        let mut opts = base_options();
        opts.key.show_ignored = false;
        let hidden_snapshot = build_snapshot(&tree.path_str(), &opts, &expanded);
        let hidden_names: Vec<_> = hidden_snapshot
            .visible
            .iter()
            .map(|id| hidden_snapshot.nodes[*id].name.as_str())
            .collect();

        assert!(hidden_names.contains(&"kept.txt"));
        assert!(!hidden_names.contains(&"ignored.txt"));
        assert!(!hidden_names.contains(&"target"));
        assert!(!hidden_names.contains(&"module.pyc"));
    }
}
