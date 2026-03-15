use mlua::prelude::*;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use super::project_fs::{WalkOptions, walk_files};

struct ManifestEntry {
    files: Vec<String>,
    dirty: Arc<AtomicBool>,
    _watcher: RecommendedWatcher,
}

static MANIFESTS: Lazy<Mutex<HashMap<String, ManifestEntry>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

fn normalize_root(root: &str) -> String {
    root.replace('\\', "/")
}

fn build_files(root: &str, max_size_bytes: Option<u64>) -> Vec<String> {
    walk_files(
        &[root.to_string()],
        &WalkOptions {
            show_hidden: false,
            max_size_bytes,
            path_glob: None,
        },
    )
}

fn ensure_manifest(root: &str, max_size_bytes: Option<u64>) -> LuaResult<()> {
    let root = normalize_root(root);
    let needs_build = {
        let manifests = MANIFESTS.lock();
        match manifests.get(&root) {
            Some(entry) => entry.dirty.load(Ordering::Relaxed),
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

    let files = build_files(&root, max_size_bytes);
    dirty.store(false, Ordering::Relaxed);
    MANIFESTS.lock().insert(
        root,
        ManifestEntry {
            files,
            dirty,
            _watcher: watcher,
        },
    );
    Ok(())
}

pub fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    let module = lua.create_table()?;

    module.set(
        "get_files",
        lua.create_function(|lua, (root, opts): (String, Option<LuaTable>)| {
            let max_size_bytes = if let Some(opts) = opts {
                opts.get::<Option<u64>>("max_size_bytes")?
            } else {
                None
            };
            ensure_manifest(&root, max_size_bytes)?;
            let root = normalize_root(&root);
            let out = lua.create_table()?;
            if let Some(entry) = MANIFESTS.lock().get(&root) {
                for (idx, file) in entry.files.iter().enumerate() {
                    out.raw_set((idx + 1) as i64, file.as_str())?;
                }
            }
            Ok(out)
        })?,
    )?;

    module.set(
        "invalidate",
        lua.create_function(|_, root: String| {
            Ok(MANIFESTS.lock().remove(&normalize_root(&root)).is_some())
        })?,
    )?;

    module.set(
        "clear_all",
        lua.create_function(|_, ()| {
            MANIFESTS.lock().clear();
            Ok(true)
        })?,
    )?;

    Ok(module)
}
