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
use std::time::{Duration, Instant};

use super::project_fs::{WalkOptions, walk_files};

/// Minimum quiet time after the last filesystem event before triggering a
/// file-list rebuild. Prevents rapid-fire rebuilds during active builds.
const REBUILD_DEBOUNCE: Duration = Duration::from_millis(500);

#[cfg(feature = "sdl")]
/// Maximum rate at which dirty-flag watcher events wake up the render loop.
/// Prevents thousands of FSEvents callbacks per second from flooding SDL.
const WAKEUP_RATE_LIMIT: Duration = Duration::from_millis(200);

struct ProjectEntry {
    /// Current (possibly stale while rebuilding) file list.
    files: Arc<Mutex<Vec<String>>>,
    /// Set by the fs watcher when any change is detected.
    dirty: Arc<AtomicBool>,
    /// True while a background rebuild is in flight.
    rebuilding: Arc<AtomicBool>,
    /// Time of the most recent filesystem event, used for debounce.
    last_event: Arc<Mutex<Option<Instant>>>,
    /// Kept alive to continue receiving notifications.
    _watcher: Arc<Mutex<Option<RecommendedWatcher>>>,
    max_size_bytes: Option<u64>,
    max_files: Option<usize>,
    exclude_dirs: Vec<String>,
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

fn build_files(
    root: &str,
    max_size_bytes: Option<u64>,
    max_files: Option<usize>,
    exclude_dirs: &[String],
) -> Vec<String> {
    walk_files(
        &[root.to_string()],
        &WalkOptions {
            show_hidden: false,
            max_size_bytes,
            path_glob: None,
            max_files,
            max_entries: None,
            exclude_dirs: exclude_dirs.to_vec(),
        },
    )
}

/// Ensure the file list for `root` is up-to-date.
///
/// Returns immediately in all cases. If a rebuild is needed it is dispatched
/// to a background thread so the Lua main thread is never blocked by I/O.
/// Callers receive the current (possibly stale) list via `get_files`.
fn ensure_project(
    root: &str,
    max_size_bytes: Option<u64>,
    max_files: Option<usize>,
    exclude_dirs: Vec<String>,
) -> LuaResult<()> {
    let root = normalize_path(root);

    enum Work {
        None,
        Rebuild {
            files: Arc<Mutex<Vec<String>>>,
            rebuilding: Arc<AtomicBool>,
            exclude_dirs: Vec<String>,
        },
        NewProject {
            files: Arc<Mutex<Vec<String>>>,
            dirty: Arc<AtomicBool>,
            rebuilding: Arc<AtomicBool>,
            last_event: Arc<Mutex<Option<Instant>>>,
            watcher: Arc<Mutex<Option<RecommendedWatcher>>>,
            exclude_dirs: Vec<String>,
        },
    }

    // Determine what work to do while holding the PROJECTS lock, then release
    // the lock before spawning any threads to avoid contention.
    let work = {
        let mut projects = PROJECTS.lock();
        match projects.get_mut(&root) {
            Some(entry) => {
                let config_changed = entry.max_size_bytes != max_size_bytes
                    || entry.max_files != max_files
                    || entry.exclude_dirs != exclude_dirs;
                let is_dirty = entry.dirty.load(Ordering::Relaxed);

                if entry.rebuilding.load(Ordering::Relaxed) {
                    Work::None
                } else if !config_changed && !is_dirty {
                    Work::None
                } else {
                    // Debounce: skip the rebuild if the last fs event was very
                    // recent, unless the configuration itself changed.
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
                    entry.max_files = max_files;
                    let ed = exclude_dirs.clone();
                    entry.exclude_dirs = exclude_dirs;
                    Work::Rebuild {
                        files: Arc::clone(&entry.files),
                        rebuilding: Arc::clone(&entry.rebuilding),
                        exclude_dirs: ed,
                    }
                }
            }
            None => {
                let files = Arc::new(Mutex::new(Vec::<String>::new()));
                let dirty = Arc::new(AtomicBool::new(false));
                let rebuilding = Arc::new(AtomicBool::new(true));
                let last_event = Arc::new(Mutex::new(None::<Instant>));
                let watcher = Arc::new(Mutex::new(None::<RecommendedWatcher>));
                projects.insert(
                    root.clone(),
                    ProjectEntry {
                        files: Arc::clone(&files),
                        dirty: Arc::clone(&dirty),
                        rebuilding: Arc::clone(&rebuilding),
                        last_event: Arc::clone(&last_event),
                        _watcher: Arc::clone(&watcher),
                        max_size_bytes,
                        max_files,
                        exclude_dirs: exclude_dirs.clone(),
                    },
                );
                Work::NewProject {
                    files,
                    dirty,
                    rebuilding,
                    last_event,
                    watcher,
                    exclude_dirs,
                }
            }
        }
    };
    // PROJECTS lock released here.

    match work {
        Work::None => {}

        Work::Rebuild {
            files: files_arc,
            rebuilding: rebuilding_arc,
            exclude_dirs,
        } => {
            let root_clone = root;
            std::thread::spawn(move || {
                let new_files = build_files(&root_clone, max_size_bytes, max_files, &exclude_dirs);
                *files_arc.lock() = new_files;
                rebuilding_arc.store(false, Ordering::Relaxed);
                #[cfg(feature = "sdl")]
                crate::window::push_wakeup_event();
            });
        }

        Work::NewProject {
            files,
            dirty,
            rebuilding,
            last_event,
            watcher: watcher_holder,
            exclude_dirs,
        } => {
            let dirty_for_cb = Arc::clone(&dirty);
            let last_event_for_cb = Arc::clone(&last_event);
            #[cfg(feature = "sdl")]
            let last_wakeup_for_cb: Arc<Mutex<Option<Instant>>> =
                Arc::new(Mutex::new(None::<Instant>));
            let root_clone = root;
            std::thread::spawn(move || {
                // Step 1: walk the project tree and populate the file list.
                // The UI can show results as soon as this completes.
                let new_files =
                    build_files(&root_clone, max_size_bytes, max_files, &exclude_dirs);
                *files.lock() = new_files;
                rebuilding.store(false, Ordering::Relaxed);
                #[cfg(feature = "sdl")]
                crate::window::push_wakeup_event();

                // Step 2: set up the recursive watcher. This is intentionally
                // done after the walk — on large trees, inotify setup can take
                // seconds, and we do not want it to delay the file list.
                let watcher_result = (|| -> Result<RecommendedWatcher, notify::Error> {
                    let mut w = RecommendedWatcher::new(
                        move |_res: notify::Result<notify::Event>| {
                            dirty_for_cb.store(true, Ordering::Relaxed);
                            *last_event_for_cb.lock() = Some(Instant::now());
                            // Rate-limit wakeup pushes to avoid flooding the
                            // render loop during active builds that generate
                            // thousands of filesystem events per second.
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

fn parse_exclude_dirs(opts: &LuaTable) -> LuaResult<Vec<String>> {
    let mut dirs = Vec::new();
    if let Some(table) = opts.get::<Option<LuaTable>>("exclude_dirs")? {
        for dir in table.sequence_values::<String>() {
            dirs.push(dir?);
        }
    }
    Ok(dirs)
}

pub fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    let module = lua.create_table()?;

    module.set(
        "sync_roots",
        lua.create_function(|_, (roots, opts): (LuaTable, Option<LuaTable>)| {
            let (max_size_bytes, max_files, exclude_dirs) = if let Some(ref opts) = opts {
                (
                    opts.get::<Option<u64>>("max_size_bytes")?,
                    opts.get::<Option<usize>>("max_files")?,
                    parse_exclude_dirs(opts)?,
                )
            } else {
                (None, None, Vec::new())
            };
            let mut keep = HashMap::new();
            for root in roots.sequence_values::<String>() {
                let root = normalize_path(&root?);
                ensure_project(&root, max_size_bytes, max_files, exclude_dirs.clone())?;
                keep.insert(root, true);
            }
            PROJECTS.lock().retain(|root, _| keep.contains_key(root));
            Ok(true)
        })?,
    )?;

    module.set(
        "get_files",
        lua.create_function(|lua, (root, opts): (String, Option<LuaTable>)| {
            let (max_size_bytes, max_files, exclude_dirs) = if let Some(ref opts) = opts {
                (
                    opts.get::<Option<u64>>("max_size_bytes")?,
                    opts.get::<Option<usize>>("max_files")?,
                    parse_exclude_dirs(opts)?,
                )
            } else {
                (None, None, Vec::new())
            };
            ensure_project(&root, max_size_bytes, max_files, exclude_dirs)?;
            let root = normalize_path(&root);
            // Clone the Arc before releasing PROJECTS to avoid holding two
            // locks simultaneously (PROJECTS then files).
            let files_arc = PROJECTS.lock().get(&root).map(|e| Arc::clone(&e.files));
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
        "get_all_files",
        lua.create_function(|lua, (roots, opts): (LuaTable, Option<LuaTable>)| {
            let (max_size_bytes, max_files, exclude_dirs) = if let Some(ref opts) = opts {
                (
                    opts.get::<Option<u64>>("max_size_bytes")?,
                    opts.get::<Option<usize>>("max_files")?,
                    parse_exclude_dirs(opts)?,
                )
            } else {
                (None, None, Vec::new())
            };
            let out = lua.create_table()?;
            let mut out_idx = 1i64;
            for root in roots.sequence_values::<String>() {
                let root = root?;
                ensure_project(&root, max_size_bytes, max_files, exclude_dirs.clone())?;
                let normalized_root = normalize_path(&root);
                let files_arc = PROJECTS
                    .lock()
                    .get(&normalized_root)
                    .map(|e| Arc::clone(&e.files));
                if let Some(files_arc) = files_arc {
                    let files = files_arc.lock();
                    for file in files.iter() {
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
