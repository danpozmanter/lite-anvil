use super::project_fs::{self, WalkOptions};
use mlua::prelude::*;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use pcre2::bytes::Regex;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use crossbeam_channel::{Receiver, Sender, unbounded};

#[derive(Clone)]
struct SearchHit {
    file: String,
    text: String,
    line: usize,
    col: usize,
}

enum SearchMsg {
    Batch(Vec<SearchHit>),
    Done,
    Error(String),
}

struct SearchHandle {
    rx: Receiver<SearchMsg>,
    cancel: Arc<AtomicBool>,
    done: bool,
}

enum ReplaceMsg {
    Done {
        replaced_count: usize,
        replaced_files: usize,
    },
    Error(String),
}

struct ReplaceHandle {
    rx: Receiver<ReplaceMsg>,
    cancel: Arc<AtomicBool>,
    done: bool,
}

static SEARCH_HANDLES: Lazy<Mutex<HashMap<u64, SearchHandle>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
static REPLACE_HANDLES: Lazy<Mutex<HashMap<u64, ReplaceHandle>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
static NEXT_SEARCH_ID: Lazy<Mutex<u64>> = Lazy::new(|| Mutex::new(1));
static NEXT_REPLACE_ID: Lazy<Mutex<u64>> = Lazy::new(|| Mutex::new(1));

#[derive(Clone)]
enum SearchMode {
    Plain,
    Regex,
    Fuzzy,
}

#[derive(Clone)]
struct SearchOpts {
    files: Vec<String>,
    query: String,
    mode: SearchMode,
    no_case: bool,
}

#[derive(Clone)]
enum ReplaceMode {
    Plain,
    Regex,
    Swap,
}

#[derive(Clone)]
struct ReplaceOpts {
    files: Vec<String>,
    mode: ReplaceMode,
    query: String,
    replace: String,
    no_case: bool,
    backup_originals: bool,
    query_b: Option<String>,
    query_b_regex: bool,
    query_b_case: bool,
    query_a_regex: bool,
}

fn next_search_id() -> u64 {
    let mut next = NEXT_SEARCH_ID.lock();
    let id = *next;
    *next += 1;
    id
}

fn next_replace_id() -> u64 {
    let mut next = NEXT_REPLACE_ID.lock();
    let id = *next;
    *next += 1;
    id
}

fn preview_text(line: &str, start_col: usize) -> String {
    let start_index = start_col.saturating_sub(80).max(1);
    let mut text = if start_index > 1 {
        format!(
            "...{}",
            &line[start_index - 1..line.len().min(256 + start_index - 1)]
        )
    } else {
        line[..line.len().min(256 + start_index - 1)].to_string()
    };
    if line.len() > 256 + start_index - 1 {
        text.push_str("...");
    }
    text
}

fn lower_if(text: &str, no_case: bool) -> String {
    if no_case {
        text.to_lowercase()
    } else {
        text.to_string()
    }
}

fn fuzzy_match_line(line: &str, query: &str, no_case: bool) -> Option<usize> {
    let hay = lower_if(line, no_case);
    let needle = lower_if(query, no_case);
    if needle.is_empty() {
        return None;
    }
    let mut qchars = needle.chars();
    let mut current = qchars.next()?;
    for (idx, ch) in hay.chars().enumerate() {
        if ch == current {
            if let Some(next) = qchars.next() {
                current = next;
            } else {
                return Some(idx + 1);
            }
        }
    }
    None
}

fn regex_find_start(re: &Regex, line: &str) -> Option<usize> {
    let mut locs = re.capture_locations();
    re.captures_read(&mut locs, line.as_bytes())
        .ok()
        .flatten()?;
    let (s, _) = locs.get(0)?;
    Some(s + 1)
}

fn search_file(path: &str, opts: &SearchOpts) -> Result<Vec<SearchHit>, String> {
    let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let regex = match opts.mode {
        SearchMode::Regex => Some(Regex::new(&opts.query).map_err(|e| e.to_string())?),
        _ => None,
    };

    let needle = if matches!(opts.mode, SearchMode::Plain) {
        Some(lower_if(&opts.query, opts.no_case))
    } else {
        None
    };

    let mut hits = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        let found = match opts.mode {
            SearchMode::Plain => {
                let hay = lower_if(line, opts.no_case);
                needle
                    .as_ref()
                    .and_then(|needle| hay.find(needle).map(|i| i + 1))
            }
            SearchMode::Regex => regex.as_ref().and_then(|re| regex_find_start(re, line)),
            SearchMode::Fuzzy => fuzzy_match_line(line, &opts.query, opts.no_case),
        };
        if let Some(col) = found {
            hits.push(SearchHit {
                file: path.to_string(),
                text: preview_text(line, col),
                line: idx + 1,
                col,
            });
        }
    }
    Ok(hits)
}

fn parse_search_mode(mode: &str) -> SearchMode {
    match mode {
        "regex" => SearchMode::Regex,
        "fuzzy" => SearchMode::Fuzzy,
        _ => SearchMode::Plain,
    }
}

fn parse_search_opts(opts: LuaTable) -> LuaResult<SearchOpts> {
    let mut files = Vec::new();
    let files_table: LuaTable = opts.get("files")?;
    for file in files_table.sequence_values::<String>() {
        files.push(file?);
    }
    Ok(SearchOpts {
        files,
        query: opts.get("query")?,
        mode: parse_search_mode(&opts.get::<String>("mode")?),
        no_case: opts.get::<Option<bool>>("no_case")?.unwrap_or(false),
    })
}

fn push_batch(tx: &Sender<SearchMsg>, batch: &mut Vec<SearchHit>) {
    if !batch.is_empty() {
        let pending = std::mem::take(batch);
        let _ = tx.send(SearchMsg::Batch(pending));
    }
}

fn parse_fs_opts(opts: Option<LuaTable>) -> LuaResult<WalkOptions> {
    let mut out = WalkOptions::default();
    if let Some(opts) = opts {
        out.show_hidden = opts.get::<Option<bool>>("show_hidden")?.unwrap_or(false);
        out.max_size_bytes = opts.get::<Option<u64>>("max_size_bytes")?;
        out.path_glob = opts.get::<Option<String>>("path_glob")?;
        out.max_files = opts.get::<Option<usize>>("max_files")?;
        if let Some(dirs) = opts.get::<Option<LuaTable>>("exclude_dirs")? {
            for dir in dirs.sequence_values::<String>() {
                out.exclude_dirs.push(dir?);
            }
        }
    }
    Ok(out)
}

fn parse_replace_opts(opts: LuaTable) -> LuaResult<ReplaceOpts> {
    let mut files = Vec::new();
    let files_table: LuaTable = opts.get("files")?;
    for file in files_table.sequence_values::<String>() {
        files.push(file?);
    }
    let mode = match opts.get::<String>("mode")?.as_str() {
        "regex" => ReplaceMode::Regex,
        "swap" => ReplaceMode::Swap,
        _ => ReplaceMode::Plain,
    };
    Ok(ReplaceOpts {
        files,
        mode,
        query: opts.get("query")?,
        replace: opts.get("replace")?,
        no_case: opts.get::<Option<bool>>("no_case")?.unwrap_or(false),
        backup_originals: opts
            .get::<Option<bool>>("backup_originals")?
            .unwrap_or(false),
        query_b: opts.get::<Option<String>>("query_b")?,
        query_b_regex: opts.get::<Option<bool>>("query_b_regex")?.unwrap_or(false),
        query_b_case: opts.get::<Option<bool>>("query_b_case")?.unwrap_or(true),
        query_a_regex: opts.get::<Option<bool>>("query_a_regex")?.unwrap_or(false),
    })
}

fn plain_find(content: &str, query: &str, no_case: bool, start: usize) -> Option<(usize, usize)> {
    let hay = if no_case {
        content.to_lowercase()
    } else {
        content.to_string()
    };
    let needle = if no_case {
        query.to_lowercase()
    } else {
        query.to_string()
    };
    hay[start.saturating_sub(1)..].find(&needle).map(|idx| {
        let s = start + idx;
        let e = s + needle.len();
        (s, e)
    })
}

fn regex_find(content: &str, re: &Regex, start: usize) -> Option<(usize, usize)> {
    let mut locs = re.capture_locations();
    re.captures_read_at(&mut locs, content.as_bytes(), start.saturating_sub(1))
        .ok()
        .flatten()?;
    let (s, e) = locs.get(0)?;
    Some((s + 1, e + 1))
}

fn replace_all_plain(content: &str, query: &str, replace: &str, no_case: bool) -> (String, usize) {
    let mut parts = Vec::new();
    let mut count = 0usize;
    let mut pos = 1usize;
    while let Some((s, e)) = plain_find(content, query, no_case, pos) {
        parts.push(content[pos - 1..s - 1].to_string());
        parts.push(replace.to_string());
        pos = e;
        count += 1;
    }
    parts.push(content[pos - 1..].to_string());
    (parts.concat(), count)
}

fn replace_all_regex(
    content: &str,
    query: &str,
    replace: &str,
    no_case: bool,
) -> Result<(String, usize), String> {
    let pat = if no_case {
        format!("(?i:{query})")
    } else {
        query.to_string()
    };
    let re = Regex::new(&pat).map_err(|e| e.to_string())?;
    let mut parts = Vec::new();
    let mut count = 0usize;
    let mut pos = 1usize;
    while let Some((s, e)) = regex_find(content, &re, pos) {
        parts.push(content[pos - 1..s - 1].to_string());
        parts.push(replace.to_string());
        count += 1;
        if e > s {
            pos = e;
        } else {
            parts.push(content[s - 1..s].to_string());
            pos = s + 1;
        }
    }
    parts.push(content[pos - 1..].to_string());
    Ok((parts.concat(), count))
}

fn generate_placeholder(content: &str, salt: usize) -> String {
    let mut counter = salt;
    loop {
        let placeholder = format!("__LITE_ANVIL_SWAP_{counter:016x}__");
        if !content.contains(&placeholder) {
            return placeholder;
        }
        counter += 1;
    }
}

fn replace_with_matcher(
    content: &str,
    query: &str,
    regex_mode: bool,
    no_case: bool,
    replace: &str,
) -> Result<(String, usize), String> {
    if regex_mode {
        replace_all_regex(content, query, replace, no_case)
    } else {
        Ok(replace_all_plain(content, query, replace, no_case))
    }
}

fn swap_content(opts: &ReplaceOpts, content: &str) -> Result<(String, usize), String> {
    let query_b = opts
        .query_b
        .clone()
        .ok_or_else(|| "missing swap B query".to_string())?;
    let placeholder = generate_placeholder(content, content.len());
    let (after_a, count_a) = replace_with_matcher(
        content,
        &opts.query,
        opts.query_a_regex,
        opts.no_case,
        &placeholder,
    )?;

    let mut parts = Vec::new();
    let mut count_b = 0usize;
    let mut pos = 0usize;
    while let Some(rel) = after_a[pos..].find(&placeholder) {
        let start = pos + rel;
        let segment = &after_a[pos..start];
        let (new_segment, seg_count) = replace_with_matcher(
            segment,
            &query_b,
            opts.query_b_regex,
            !opts.query_b_case,
            &opts.query,
        )?;
        count_b += seg_count;
        parts.push(new_segment);
        parts.push(placeholder.clone());
        pos = start + placeholder.len();
    }
    let tail = &after_a[pos..];
    let (tail_new, tail_count) = replace_with_matcher(
        tail,
        &query_b,
        opts.query_b_regex,
        !opts.query_b_case,
        &opts.query,
    )?;
    count_b += tail_count;
    parts.push(tail_new);
    Ok((
        parts.concat().replace(&placeholder, &opts.replace),
        count_a + count_b,
    ))
}

pub fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    let module = lua.create_table()?;

    module.set(
        "collect_files",
        lua.create_function(|lua, (roots, opts): (LuaTable, Option<LuaTable>)| {
            let fs_opts = parse_fs_opts(opts)?;
            let mut root_list = Vec::new();
            for root in roots.sequence_values::<String>() {
                root_list.push(root?);
            }
            let files = project_fs::walk_files(&root_list, &fs_opts);
            let out = lua.create_table_with_capacity(files.len(), 0)?;
            for (idx, path) in files.into_iter().enumerate() {
                out.raw_set((idx + 1) as i64, path)?;
            }
            Ok(out)
        })?,
    )?;

    module.set(
        "search",
        lua.create_function(|_, opts: LuaTable| {
            let opts = parse_search_opts(opts)?;
            let (tx, rx) = unbounded::<SearchMsg>();
            let cancel = Arc::new(AtomicBool::new(false));
            let cancel_thread = Arc::clone(&cancel);
            std::thread::spawn(move || {
                let mut batch = Vec::new();
                for file in &opts.files {
                    if cancel_thread.load(Ordering::Relaxed) {
                        break;
                    }
                    match search_file(file, &opts) {
                        Ok(mut hits) => {
                            batch.append(&mut hits);
                            if batch.len() >= 64 {
                                push_batch(&tx, &mut batch);
                            }
                        }
                        Err(err) => {
                            let _ = tx.send(SearchMsg::Error(format!("{file}: {err}")));
                        }
                    }
                }
                push_batch(&tx, &mut batch);
                let _ = tx.send(SearchMsg::Done);
                #[cfg(feature = "sdl")]
                crate::window::push_wakeup_event();
            });
            let id = next_search_id();
            SEARCH_HANDLES.lock().insert(
                id,
                SearchHandle {
                    rx,
                    cancel,
                    done: false,
                },
            );
            Ok(id)
        })?,
    )?;

    module.set(
        "poll",
        lua.create_function(|lua, (handle_id, max_items): (u64, Option<usize>)| {
            let max_items = max_items.unwrap_or(256);
            let mut handles = SEARCH_HANDLES.lock();
            let Some(handle) = handles.get_mut(&handle_id) else {
                return Ok(LuaValue::Nil);
            };

            let out = lua.create_table()?;
            let results = lua.create_table()?;
            let mut idx = 1i64;
            let mut pulled = 0usize;
            let mut error = None::<String>;

            while pulled < max_items {
                match handle.rx.try_recv() {
                    Ok(SearchMsg::Batch(batch)) => {
                        for hit in batch {
                            let item = lua.create_table()?;
                            item.set("file", hit.file)?;
                            item.set("text", hit.text)?;
                            item.set("line", hit.line)?;
                            item.set("col", hit.col)?;
                            results.raw_set(idx, item)?;
                            idx += 1;
                            pulled += 1;
                            if pulled >= max_items {
                                break;
                            }
                        }
                    }
                    Ok(SearchMsg::Done) => {
                        handle.done = true;
                        break;
                    }
                    Ok(SearchMsg::Error(err)) => {
                        error = Some(err);
                        break;
                    }
                    Err(_) => break,
                }
            }

            out.set("results", results)?;
            out.set("done", handle.done)?;
            if let Some(err) = error {
                out.set("error", err)?;
                handle.done = true;
            }
            if handle.done {
                handles.remove(&handle_id);
            }
            Ok(LuaValue::Table(out))
        })?,
    )?;

    module.set(
        "cancel",
        lua.create_function(|_, handle_id: u64| {
            if let Some(handle) = SEARCH_HANDLES.lock().remove(&handle_id) {
                handle.cancel.store(true, Ordering::Relaxed);
                Ok(true)
            } else {
                Ok(false)
            }
        })?,
    )?;

    module.set(
        "replace",
        lua.create_function(|lua, opts: LuaTable| {
            let opts = parse_replace_opts(opts)?;
            let mut replaced_count = 0usize;
            let mut replaced_files = 0usize;
            for file in &opts.files {
                let path = Path::new(file);
                let Ok(content) = fs::read_to_string(path) else {
                    continue;
                };
                let result = match opts.mode {
                    ReplaceMode::Plain => Ok(replace_all_plain(
                        &content,
                        &opts.query,
                        &opts.replace,
                        opts.no_case,
                    )),
                    ReplaceMode::Regex => {
                        replace_all_regex(&content, &opts.query, &opts.replace, opts.no_case)
                    }
                    ReplaceMode::Swap => swap_content(&opts, &content),
                };
                let (new_content, count) = result.map_err(LuaError::RuntimeError)?;
                if count == 0 {
                    continue;
                }
                if opts.backup_originals {
                    if let Err(e) = fs::write(format!("{file}.bak"), &content) {
                        log::warn!("failed to write backup {file}.bak: {e}");
                    }
                }
                fs::write(path, new_content).map_err(|e| LuaError::RuntimeError(e.to_string()))?;
                replaced_count += count;
                replaced_files += 1;
            }
            let out = lua.create_table()?;
            out.set("replaced_count", replaced_count)?;
            out.set("replaced_files", replaced_files)?;
            Ok(out)
        })?,
    )?;

    module.set(
        "replace_async",
        lua.create_function(|_, opts: LuaTable| {
            let opts = parse_replace_opts(opts)?;
            let (tx, rx) = unbounded::<ReplaceMsg>();
            let cancel = Arc::new(AtomicBool::new(false));
            let cancel_thread = Arc::clone(&cancel);
            std::thread::spawn(move || {
                let mut replaced_count = 0usize;
                let mut replaced_files = 0usize;
                for file in &opts.files {
                    if cancel_thread.load(Ordering::Relaxed) {
                        break;
                    }
                    let path = Path::new(file);
                    let Ok(content) = fs::read_to_string(path) else {
                        continue;
                    };
                    let result = match opts.mode {
                        ReplaceMode::Plain => Ok(replace_all_plain(
                            &content,
                            &opts.query,
                            &opts.replace,
                            opts.no_case,
                        )),
                        ReplaceMode::Regex => {
                            replace_all_regex(&content, &opts.query, &opts.replace, opts.no_case)
                        }
                        ReplaceMode::Swap => swap_content(&opts, &content),
                    };
                    let (new_content, count) = match result {
                        Ok(result) => result,
                        Err(err) => {
                            let _ = tx.send(ReplaceMsg::Error(err));
                            #[cfg(feature = "sdl")]
                            crate::window::push_wakeup_event();
                            return;
                        }
                    };
                    if count == 0 {
                        continue;
                    }
                    if opts.backup_originals {
                        if let Err(e) = fs::write(format!("{file}.bak"), &content) {
                        log::warn!("failed to write backup {file}.bak: {e}");
                    }
                    }
                    if let Err(err) = fs::write(path, new_content) {
                        let _ = tx.send(ReplaceMsg::Error(err.to_string()));
                        #[cfg(feature = "sdl")]
                        crate::window::push_wakeup_event();
                        return;
                    }
                    replaced_count += count;
                    replaced_files += 1;
                }
                let _ = tx.send(ReplaceMsg::Done {
                    replaced_count,
                    replaced_files,
                });
                #[cfg(feature = "sdl")]
                crate::window::push_wakeup_event();
            });
            let id = next_replace_id();
            REPLACE_HANDLES.lock().insert(
                id,
                ReplaceHandle {
                    rx,
                    cancel,
                    done: false,
                },
            );
            Ok(id)
        })?,
    )?;

    module.set(
        "replace_poll",
        lua.create_function(|lua, handle_id: u64| {
            let mut handles = REPLACE_HANDLES.lock();
            let Some(handle) = handles.get_mut(&handle_id) else {
                return Ok(LuaValue::Nil);
            };

            let out = lua.create_table()?;
            let mut replaced_count = None::<usize>;
            let mut replaced_files = None::<usize>;
            let mut error = None::<String>;

            while let Ok(msg) = handle.rx.try_recv() {
                match msg {
                    ReplaceMsg::Done {
                        replaced_count: count,
                        replaced_files: files,
                    } => {
                        replaced_count = Some(count);
                        replaced_files = Some(files);
                        handle.done = true;
                    }
                    ReplaceMsg::Error(err) => {
                        error = Some(err);
                        handle.done = true;
                    }
                }
            }

            out.set("done", handle.done)?;
            if let Some(count) = replaced_count {
                out.set("replaced_count", count)?;
            }
            if let Some(files) = replaced_files {
                out.set("replaced_files", files)?;
            }
            if let Some(err) = error {
                out.set("error", err)?;
            }
            if handle.done {
                handles.remove(&handle_id);
            }
            Ok(LuaValue::Table(out))
        })?,
    )?;

    module.set(
        "replace_cancel",
        lua.create_function(|_, handle_id: u64| {
            if let Some(handle) = REPLACE_HANDLES.lock().remove(&handle_id) {
                handle.cancel.store(true, Ordering::Relaxed);
                Ok(true)
            } else {
                Ok(false)
            }
        })?,
    )?;

    Ok(module)
}
