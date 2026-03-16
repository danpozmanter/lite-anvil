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
use std::time::{Duration, Instant};

use super::project_fs::{WalkOptions, walk_files};

/// Minimum quiet time after the last filesystem event before triggering a rebuild.
const REBUILD_DEBOUNCE: Duration = Duration::from_millis(500);

#[cfg(feature = "sdl")]
/// Maximum rate at which dirty-flag watcher events wake up the render loop.
const WAKEUP_RATE_LIMIT: Duration = Duration::from_millis(200);

struct ManifestEntry {
    files: Arc<Mutex<Vec<String>>>,
    dirty: Arc<AtomicBool>,
    rebuilding: Arc<AtomicBool>,
    last_event: Arc<Mutex<Option<Instant>>>,
    _watcher: Arc<Mutex<Option<RecommendedWatcher>>>,
    max_size_bytes: Option<u64>,
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
            max_files: None,
            max_entries: None,
            exclude_dirs: vec![],
        },
    )
}

/// Ensure the file list for `root` is up-to-date.
///
/// Returns immediately in all cases. If a rebuild is needed it is dispatched
/// to a background thread so the Lua main thread is never blocked by I/O.
fn ensure_manifest(root: &str, max_size_bytes: Option<u64>) -> LuaResult<()> {
    let root = normalize_root(root);

    enum Work {
        None,
        Rebuild {
            files: Arc<Mutex<Vec<String>>>,
            rebuilding: Arc<AtomicBool>,
        },
        NewManifest {
            files: Arc<Mutex<Vec<String>>>,
            dirty: Arc<AtomicBool>,
            rebuilding: Arc<AtomicBool>,
            last_event: Arc<Mutex<Option<Instant>>>,
            watcher: Arc<Mutex<Option<RecommendedWatcher>>>,
        },
    }

    let work = {
        let mut manifests = MANIFESTS.lock();
        match manifests.get_mut(&root) {
            Some(entry) => {
                let config_changed = entry.max_size_bytes != max_size_bytes;
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
                    entry.max_size_bytes = max_size_bytes;
                    Work::Rebuild {
                        files: Arc::clone(&entry.files),
                        rebuilding: Arc::clone(&entry.rebuilding),
                    }
                }
            }
            None => {
                let files = Arc::new(Mutex::new(Vec::<String>::new()));
                let dirty = Arc::new(AtomicBool::new(false));
                let rebuilding = Arc::new(AtomicBool::new(true));
                let last_event = Arc::new(Mutex::new(None::<Instant>));
                let watcher = Arc::new(Mutex::new(None::<RecommendedWatcher>));
                manifests.insert(
                    root.clone(),
                    ManifestEntry {
                        files: Arc::clone(&files),
                        dirty: Arc::clone(&dirty),
                        rebuilding: Arc::clone(&rebuilding),
                        last_event: Arc::clone(&last_event),
                        _watcher: Arc::clone(&watcher),
                        max_size_bytes,
                    },
                );
                Work::NewManifest {
                    files,
                    dirty,
                    rebuilding,
                    last_event,
                    watcher,
                }
            }
        }
    };
    // MANIFESTS lock released here.

    match work {
        Work::None => {}

        Work::Rebuild {
            files: files_arc,
            rebuilding: rebuilding_arc,
        } => {
            let root_clone = root;
            std::thread::spawn(move || {
                let new_files = build_files(&root_clone, max_size_bytes);
                *files_arc.lock() = new_files;
                rebuilding_arc.store(false, Ordering::Relaxed);
                #[cfg(feature = "sdl")]
                crate::window::push_wakeup_event();
            });
        }

        Work::NewManifest {
            files,
            dirty,
            rebuilding,
            last_event,
            watcher: watcher_holder,
        } => {
            let dirty_for_cb = Arc::clone(&dirty);
            let last_event_for_cb = Arc::clone(&last_event);
            #[cfg(feature = "sdl")]
            let last_wakeup_for_cb: Arc<Mutex<Option<Instant>>> =
                Arc::new(Mutex::new(None::<Instant>));
            let root_clone = root;
            std::thread::spawn(move || {
                // Step 1: walk the tree and populate the file list first.
                let new_files = build_files(&root_clone, max_size_bytes);
                *files.lock() = new_files;
                rebuilding.store(false, Ordering::Relaxed);
                #[cfg(feature = "sdl")]
                crate::window::push_wakeup_event();

                // Step 2: set up the recursive watcher after the walk so that
                // slow inotify registration on large trees does not delay results.
                let watcher_result = (|| -> Result<RecommendedWatcher, notify::Error> {
                    let mut w = RecommendedWatcher::new(
                        move |_res: notify::Result<notify::Event>| {
                            dirty_for_cb.store(true, Ordering::Relaxed);
                            *last_event_for_cb.lock() = Some(Instant::now());
                            // Rate-limit wakeup pushes to avoid flooding SDL.
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
                if let Ok(watcher) = watcher_result {
                    *watcher_holder.lock() = Some(watcher);
                }
            });
        }
    }

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
            // Clone the Arc before releasing MANIFESTS to avoid holding two
            // locks simultaneously (MANIFESTS then files).
            let files_arc = MANIFESTS.lock().get(&root).map(|e| Arc::clone(&e.files));
            let out = lua.create_table()?;
            if let Some(files_arc) = files_arc {
                let files = files_arc.lock();
                for (idx, file) in files.iter().enumerate() {
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
