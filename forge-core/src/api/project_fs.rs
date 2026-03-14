use mlua::prelude::*;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

fn read_dir_entries(path: &Path, show_hidden: bool) -> Vec<DirEntry> {
    let mut entries = Vec::new();
    if let Ok(read_dir) = fs::read_dir(path) {
        for entry in read_dir.flatten() {
            let entry_path = entry.path();
            if !show_hidden && is_hidden(&entry_path) {
                continue;
            }
            if let Ok(meta) = entry.metadata() {
                let kind = if meta.is_dir() { "dir" } else { "file" }.to_string();
                entries.push(DirEntry {
                    name: entry.file_name().to_string_lossy().into_owned(),
                    abs_path: entry_path.to_string_lossy().into_owned(),
                    kind,
                    size: meta.len(),
                });
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
        let entries = read_dir_entries(&path, opts.show_hidden);
        for entry in entries {
            let entry_path = PathBuf::from(&entry.abs_path);
            if entry.kind == "dir" {
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
    }
    Ok(out)
}

pub fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    let module = lua.create_table()?;

    module.set(
        "list_dir",
        lua.create_function(|lua, (path, opts): (String, Option<LuaTable>)| {
            let opts = parse_walk_opts(opts)?;
            let entries = read_dir_entries(Path::new(&path), opts.show_hidden);
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
            let mut watcher = RecommendedWatcher::new(
                move |res: notify::Result<notify::Event>| {
                    if let Ok(event) = res {
                        let mut queue = queue_for_cb.lock();
                        for path in event.paths {
                            queue.push_back(path.to_string_lossy().into_owned());
                        }
                    }
                    #[cfg(feature = "sdl")]
                    crate::window::push_wakeup_event();
                },
                notify::Config::default(),
            )
            .map_err(|e| LuaError::RuntimeError(e.to_string()))?;
            watcher
                .watch(Path::new(&path), RecursiveMode::Recursive)
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
                let mut idx = 1i64;
                while let Some(path) = queue.pop_front() {
                    out.raw_set(idx, path)?;
                    idx += 1;
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
