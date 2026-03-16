use mlua::prelude::*;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use super::project_fs::{WalkOptions, walk_files};

struct ProjectEntry {
    files: Vec<String>,
    dirty: Arc<AtomicBool>,
    _watcher: RecommendedWatcher,
    max_size_bytes: Option<u64>,
    max_files: Option<usize>,
}

static PROJECTS: Lazy<Mutex<HashMap<String, ProjectEntry>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
}

fn join_path(root: &str, filename: &str) -> String {
    if Path::new(filename).is_absolute() {
        return normalize_path(filename);
    }
    normalize_path(&PathBuf::from(root).join(filename).to_string_lossy())
}

fn relative_to(root: &str, filename: &str) -> String {
    let root = Path::new(root);
    let filename = Path::new(filename);
    filename
        .strip_prefix(root)
        .ok()
        .and_then(|path| path.to_str())
        .map(normalize_path)
        .unwrap_or_else(|| normalize_path(&filename.to_string_lossy()))
}

fn build_files(root: &str, max_size_bytes: Option<u64>, max_files: Option<usize>) -> Vec<String> {
    walk_files(
        &[root.to_string()],
        &WalkOptions {
            show_hidden: false,
            max_size_bytes,
            path_glob: None,
            max_files,
            max_entries: None,
        },
    )
}

fn ensure_project(
    root: &str,
    max_size_bytes: Option<u64>,
    max_files: Option<usize>,
) -> LuaResult<()> {
    let root = normalize_path(root);
    let needs_build = {
        let projects = PROJECTS.lock();
        match projects.get(&root) {
            Some(entry) => {
                entry.dirty.load(Ordering::Relaxed)
                    || entry.max_size_bytes != max_size_bytes
                    || entry.max_files != max_files
            }
            None => true,
        }
    };
    if !needs_build {
        return Ok(());
    }

    let dirty = Arc::new(AtomicBool::new(false));
    let dirty_for_cb = Arc::clone(&dirty);
    let mut watcher = RecommendedWatcher::new(
        move |_res: notify::Result<notify::Event>| {
            dirty_for_cb.store(true, Ordering::Relaxed);
            #[cfg(feature = "sdl")]
            crate::window::push_wakeup_event();
        },
        notify::Config::default(),
    )
    .map_err(|e| LuaError::RuntimeError(e.to_string()))?;
    watcher
        .watch(Path::new(&root), RecursiveMode::Recursive)
        .map_err(|e| LuaError::RuntimeError(e.to_string()))?;

    let files = build_files(&root, max_size_bytes, max_files);
    dirty.store(false, Ordering::Relaxed);
    PROJECTS.lock().insert(
        root,
        ProjectEntry {
            files,
            dirty,
            _watcher: watcher,
            max_size_bytes,
            max_files,
        },
    );
    Ok(())
}

pub fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    let module = lua.create_table()?;

    module.set(
        "sync_roots",
        lua.create_function(|_, (roots, opts): (LuaTable, Option<LuaTable>)| {
            let (max_size_bytes, max_files) = if let Some(opts) = opts {
                (
                    opts.get::<Option<u64>>("max_size_bytes")?,
                    opts.get::<Option<usize>>("max_files")?,
                )
            } else {
                (None, None)
            };
            let mut keep = HashMap::new();
            for root in roots.sequence_values::<String>() {
                let root = normalize_path(&root?);
                ensure_project(&root, max_size_bytes, max_files)?;
                keep.insert(root, true);
            }
            PROJECTS.lock().retain(|root, _| keep.contains_key(root));
            Ok(true)
        })?,
    )?;

    module.set(
        "get_files",
        lua.create_function(|lua, (root, opts): (String, Option<LuaTable>)| {
            let (max_size_bytes, max_files) = if let Some(opts) = opts {
                (
                    opts.get::<Option<u64>>("max_size_bytes")?,
                    opts.get::<Option<usize>>("max_files")?,
                )
            } else {
                (None, None)
            };
            ensure_project(&root, max_size_bytes, max_files)?;
            let root = normalize_path(&root);
            let out = lua.create_table()?;
            if let Some(entry) = PROJECTS.lock().get(&root) {
                for (idx, file) in entry.files.iter().enumerate() {
                    out.raw_set((idx + 1) as i64, file.as_str())?;
                }
            }
            Ok(out)
        })?,
    )?;

    module.set(
        "get_all_files",
        lua.create_function(|lua, (roots, opts): (LuaTable, Option<LuaTable>)| {
            let (max_size_bytes, max_files) = if let Some(opts) = opts {
                (
                    opts.get::<Option<u64>>("max_size_bytes")?,
                    opts.get::<Option<usize>>("max_files")?,
                )
            } else {
                (None, None)
            };
            let out = lua.create_table()?;
            let mut out_idx = 1i64;
            for root in roots.sequence_values::<String>() {
                let root = root?;
                ensure_project(&root, max_size_bytes, max_files)?;
                let normalized_root = normalize_path(&root);
                if let Some(entry) = PROJECTS.lock().get(&normalized_root) {
                    for file in &entry.files {
                        out.raw_set(out_idx, file.as_str())?;
                        out_idx += 1;
                    }
                }
            }
            Ok(out)
        })?,
    )?;

    module.set(
        "absolute_path",
        lua.create_function(|_, (root, filename): (String, String)| {
            Ok(join_path(&root, &filename))
        })?,
    )?;

    module.set(
        "normalize_path",
        lua.create_function(|_, (root, filename): (String, String)| {
            let root = normalize_path(&root);
            let filename = normalize_path(&filename);
            Ok(relative_to(&root, &filename))
        })?,
    )?;

    module.set(
        "invalidate",
        lua.create_function(|_, root: String| {
            Ok(PROJECTS.lock().remove(&normalize_path(&root)).is_some())
        })?,
    )?;

    Ok(module)
}

#[cfg(test)]
mod tests {
    use super::{join_path, relative_to};

    #[test]
    fn joins_relative_paths() {
        assert!(join_path("/tmp/project", "src/main.rs").ends_with("/tmp/project/src/main.rs"));
    }

    #[test]
    fn normalizes_relative_paths() {
        assert_eq!(
            relative_to("/tmp/project", "/tmp/project/src/main.rs"),
            "src/main.rs"
        );
    }
}
