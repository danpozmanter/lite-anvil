use mlua::prelude::*;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const MAX_QUEUED_CHANGES: usize = 4096;

struct WatchHandle {
    _watcher: RecommendedWatcher,
    queue: Arc<Mutex<VecDeque<String>>>,
}

static WATCHERS: Lazy<Mutex<HashMap<u64, WatchHandle>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static NEXT_WATCH_ID: Lazy<Mutex<u64>> = Lazy::new(|| Mutex::new(1));

#[derive(Default)]
pub(crate) struct WalkOptions {
    pub show_hidden: bool,
    pub max_size_bytes: Option<u64>,
    pub path_glob: Option<String>,
    pub max_files: Option<usize>,
    pub max_entries: Option<usize>,
    /// Directory basenames to skip entirely during traversal.
    pub exclude_dirs: Vec<String>,
}

fn next_watch_id() -> u64 {
    let mut next = NEXT_WATCH_ID.lock();
    let id = *next;
    *next += 1;
    id
}

fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.starts_with('.'))
        .unwrap_or(false)
}

fn glob_matches(path: &str, glob: &str) -> bool {
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

fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .ok()
        .and_then(|p| p.to_str())
        .unwrap_or("")
        .replace('\\', "/")
}

fn sort_entries(entries: &mut [DirEntry]) {
    entries.sort_by(|a, b| {
        let a_type = if a.kind == "dir" { "dir" } else { "file" };
        let b_type = if b.kind == "dir" { "dir" } else { "file" };
        if super::path_compare(&a.name, a_type, &b.name, b_type) {
            std::cmp::Ordering::Less
        } else if super::path_compare(&b.name, b_type, &a.name, a_type) {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Equal
        }
    });
}

#[derive(Clone)]
struct DirEntry {
    name: String,
    abs_path: String,
    kind: String,
    size: u64,
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
            let kind = if file_type.is_dir() { "dir" } else { "file" }.to_string();
            let size = if file_type.is_file() {
                entry.metadata().map(|meta| meta.len()).unwrap_or(0)
            } else {
                0
            };
            entries.push(DirEntry {
                name: entry.file_name().to_string_lossy().into_owned(),
                abs_path: entry_path.to_string_lossy().into_owned(),
                kind,
                size,
            });
            if max_entries.is_some_and(|limit| entries.len() >= limit) {
                break;
            }
        }
    }
    sort_entries(&mut entries);
    entries
}

pub(crate) fn walk_files(roots: &[String], opts: &WalkOptions) -> Vec<String> {
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
            if entry.kind == "dir" {
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

fn parse_walk_opts(opts: Option<LuaTable>) -> LuaResult<WalkOptions> {
    let mut out = WalkOptions::default();
    if let Some(opts) = opts {
        out.show_hidden = opts.get::<Option<bool>>("show_hidden")?.unwrap_or(false);
        out.max_size_bytes = opts.get::<Option<u64>>("max_size_bytes")?;
        out.path_glob = opts.get::<Option<String>>("path_glob")?;
        out.max_files = opts.get::<Option<usize>>("max_files")?;
        out.max_entries = opts.get::<Option<usize>>("max_entries")?;
        if let Some(dirs) = opts.get::<Option<LuaTable>>("exclude_dirs")? {
            for dir in dirs.sequence_values::<String>() {
                out.exclude_dirs.push(dir?);
            }
        }
    }
    Ok(out)
}

pub fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    let module = lua.create_table()?;

    module.set(
        "list_dir",
        lua.create_function(|lua, (path, opts): (String, Option<LuaTable>)| {
            let opts = parse_walk_opts(opts)?;
            let entries = read_dir_entries(Path::new(&path), opts.show_hidden, opts.max_entries);
            let out = lua.create_table_with_capacity(entries.len(), 0)?;
            for (idx, entry) in entries.into_iter().enumerate() {
                let item = lua.create_table()?;
                item.set("name", entry.name)?;
                item.set("abs_filename", entry.abs_path)?;
                item.set("type", entry.kind)?;
                item.set("size", entry.size)?;
                out.raw_set((idx + 1) as i64, item)?;
            }
            Ok(out)
        })?,
    )?;

    module.set(
        "walk_files",
        lua.create_function(|lua, (roots, opts): (LuaTable, Option<LuaTable>)| {
            let opts = parse_walk_opts(opts)?;
            let mut root_list = Vec::new();
            for root in roots.sequence_values::<String>() {
                root_list.push(root?);
            }
            let files = walk_files(&root_list, &opts);
            let out = lua.create_table_with_capacity(files.len(), 0)?;
            for (idx, file) in files.into_iter().enumerate() {
                out.raw_set((idx + 1) as i64, file)?;
            }
            Ok(out)
        })?,
    )?;

    module.set(
        "watch_project",
        lua.create_function(|_, path: String| {
            let queue = Arc::new(Mutex::new(VecDeque::new()));
            let queue_for_cb = Arc::clone(&queue);
            let root_path = path.clone();
            let mut watcher = RecommendedWatcher::new(
                move |res: notify::Result<notify::Event>| {
                    if let Ok(event) = res {
                        let mut queue = queue_for_cb.lock();
                        if queue.len() >= MAX_QUEUED_CHANGES {
                            queue.clear();
                            queue.push_back(root_path.clone());
                            return;
                        }
                        for path in event.paths {
                            queue.push_back(path.to_string_lossy().into_owned());
                            if queue.len() >= MAX_QUEUED_CHANGES {
                                queue.clear();
                                queue.push_back(root_path.clone());
                                break;
                            }
                        }
                    }
                },
                notify::Config::default(),
            )
            .map_err(|e| LuaError::RuntimeError(e.to_string()))?;
            watcher
                .watch(Path::new(&path), RecursiveMode::NonRecursive)
                .map_err(|e| LuaError::RuntimeError(e.to_string()))?;
            let id = next_watch_id();
            WATCHERS.lock().insert(
                id,
                WatchHandle {
                    _watcher: watcher,
                    queue,
                },
            );
            Ok(id)
        })?,
    )?;

    module.set(
        "poll_changes",
        lua.create_function(|lua, watch_id: u64| {
            let out = lua.create_table()?;
            if let Some(handle) = WATCHERS.lock().get(&watch_id) {
                let mut queue = handle.queue.lock();
                let mut seen = HashSet::new();
                let mut idx = 1i64;
                while let Some(path) = queue.pop_front() {
                    if seen.insert(path.clone()) {
                        out.raw_set(idx, path)?;
                        idx += 1;
                    }
                }
            }
            Ok(out)
        })?,
    )?;

    module.set(
        "cancel_watch",
        lua.create_function(|_, watch_id: u64| Ok(WATCHERS.lock().remove(&watch_id).is_some()))?,
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
                "lite-anvil-project-fs-{}-{}",
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

    #[test]
    fn glob_matches_supports_single_and_double_star() {
        assert!(glob_matches("src/lib.rs", "*.rs") == false);
        assert!(glob_matches("src/lib.rs", "src/*.rs"));
        assert!(glob_matches("src/nested/lib.rs", "src/**/*.rs"));
        assert!(glob_matches("src/lib.rs", "**/*.rs"));
        assert!(!glob_matches("src/lib.rs", "**/*.toml"));
    }

    #[test]
    fn read_dir_entries_sorts_directories_before_files() {
        let tree = TempTree::new();
        fs::create_dir(tree.path.join("b-dir")).unwrap();
        write_file(&tree.path.join("a-file.txt"), b"a");
        write_file(&tree.path.join("z-file.txt"), b"z");

        let entries = read_dir_entries(&tree.path, false, None);
        let names: Vec<_> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec!["b-dir", "a-file.txt", "z-file.txt"]);
    }

    #[test]
    fn walk_files_applies_hidden_glob_size_and_count_limits() {
        let tree = TempTree::new();
        write_file(&tree.path.join("visible.txt"), b"ok");
        write_file(&tree.path.join(".hidden.txt"), b"hidden");
        write_file(&tree.path.join("nested").join("keep.md"), b"keep");
        write_file(&tree.path.join("nested").join("skip.bin"), b"0123456789");

        let files = walk_files(
            &[tree.path_str()],
            &WalkOptions {
                show_hidden: false,
                max_size_bytes: Some(10),
                path_glob: Some("**/*.md".to_string()),
                max_files: Some(1),
                max_entries: None,
                exclude_dirs: vec![],
            },
        );

        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("nested/keep.md"));
    }

    #[test]
    fn walk_files_skips_excluded_directories() {
        let tree = TempTree::new();
        write_file(&tree.path.join("root.txt"), b"root");
        write_file(&tree.path.join("build").join("artifact.class"), b"class");
        write_file(&tree.path.join("__pycache__").join("module.pyc"), b"pyc");
        write_file(&tree.path.join("src").join("main.kt"), b"kt");

        let files = walk_files(
            &[tree.path_str()],
            &WalkOptions {
                show_hidden: false,
                max_size_bytes: None,
                path_glob: None,
                max_files: None,
                max_entries: None,
                exclude_dirs: vec!["build".to_string(), "__pycache__".to_string()],
            },
        );

        let names: Vec<_> = files
            .iter()
            .map(|f| std::path::Path::new(f).file_name().unwrap().to_str().unwrap())
            .collect();
        assert!(names.contains(&"root.txt"), "root.txt should be included");
        assert!(names.contains(&"main.kt"), "main.kt should be included");
        assert!(!names.contains(&"artifact.class"), "build/ should be excluded");
        assert!(!names.contains(&"module.pyc"), "__pycache__/ should be excluded");
    }
}
