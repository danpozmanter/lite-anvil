use crossbeam_channel::Receiver;
use mlua::prelude::*;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

// ── Inner state ───────────────────────────────────────────────────────────────

struct DirMonitorInner {
    watcher: RecommendedWatcher,
    rx: Receiver<notify::Result<notify::Event>>,
    path_to_id: HashMap<PathBuf, i32>,
    id_to_path: HashMap<i32, PathBuf>,
    next_id: i32,
}

pub struct DirMonitor(Mutex<DirMonitorInner>);

// ── LuaUserData methods ───────────────────────────────────────────────────────

impl LuaUserData for DirMonitor {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(LuaMetaMethod::ToString, |_, _, ()| Ok("dirmonitor"));

        // mode() → "multiple": each watched path has its own integer watch ID,
        // matching inotify semantics.
        methods.add_method("mode", |_, _, ()| -> LuaResult<&'static str> {
            Ok("multiple")
        });

        // watch(path) → watch_id (>= 0), or -1 on error.
        // Returns the existing ID if the path is already watched.
        methods.add_method("watch", |_, this, path: String| -> LuaResult<i32> {
            let mut inner = this.0.lock();
            let pb = PathBuf::from(&path);
            if let Some(&id) = inner.path_to_id.get(&pb) {
                return Ok(id);
            }
            match inner.watcher.watch(&pb, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    let id = inner.next_id;
                    inner.next_id += 1;
                    inner.path_to_id.insert(pb.clone(), id);
                    inner.id_to_path.insert(id, pb);
                    Ok(id)
                }
                Err(_) => Ok(-1),
            }
        });

        // unwatch(watch_id) → void.
        methods.add_method("unwatch", |_, this, watch_id: i32| -> LuaResult<()> {
            let mut inner = this.0.lock();
            if let Some(path) = inner.id_to_path.remove(&watch_id) {
                let _ = inner.watcher.unwatch(&path);
                inner.path_to_id.remove(&path);
            }
            Ok(())
        });

        // check(callback, [error_cb]) → false (no events) or true (callback(s) fired).
        //
        // Drains all pending notify events and calls callback(watch_id) for each unique
        // watch ID that fired. The lock is released before calling any Lua callbacks so
        // that the callback may safely call watch()/unwatch() on the same monitor.
        methods.add_method(
            "check",
            |_, this, (callback, error_cb): (LuaFunction, Option<LuaFunction>)| {
                // Collect fired IDs under the lock, then release.
                let fired_ids: Vec<i32> = {
                    let inner = this.0.lock();
                    let mut seen: HashSet<i32> = HashSet::new();
                    let mut ids: Vec<i32> = Vec::new();
                    while let Ok(Ok(event)) = inner.rx.try_recv() {
                        for path in &event.paths {
                            // Map changed path → watch ID by checking the path
                            // itself first, then its parent (for file events
                            // inside a watched directory).
                            let id = inner
                                .path_to_id
                                .get(path)
                                .or_else(|| path.parent().and_then(|p| inner.path_to_id.get(p)))
                                .copied();
                            if let Some(id) = id {
                                if seen.insert(id) {
                                    ids.push(id);
                                }
                            }
                        }
                    }
                    ids
                    // Lock released here.
                };

                if fired_ids.is_empty() {
                    return Ok(LuaValue::Boolean(false));
                }

                for id in fired_ids {
                    if let Err(e) = callback.call::<()>(id) {
                        if let Some(ref ecb) = error_cb {
                            let _ = ecb.call::<()>(e.to_string());
                        }
                    }
                }
                Ok(LuaValue::Boolean(true))
            },
        );
    }
}

// ── Module factory ────────────────────────────────────────────────────────────

fn new_monitor(_lua: &Lua, (): ()) -> LuaResult<DirMonitor> {
    let (tx, rx) = crossbeam_channel::unbounded::<notify::Result<notify::Event>>();
    let watcher = RecommendedWatcher::new(
        move |result| {
            let _ = tx.send(result);
            // Wake up the SDL event loop so changes are noticed without waiting
            // for the full frame timeout.
            #[cfg(feature = "sdl")]
            crate::window::push_wakeup_event();
        },
        notify::Config::default(),
    )
    .map_err(|e| LuaError::RuntimeError(format!("dirmonitor init: {e}")))?;

    Ok(DirMonitor(Mutex::new(DirMonitorInner {
        watcher,
        rx,
        path_to_id: HashMap::new(),
        id_to_path: HashMap::new(),
        next_id: 0,
    })))
}

pub fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    let t = lua.create_table()?;
    t.set("new", lua.create_function(new_monitor)?)?;
    Ok(t)
}
