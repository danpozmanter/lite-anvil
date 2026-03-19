use mlua::prelude::*;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

struct StatusCache {
    map: HashMap<String, u64>,
    order: VecDeque<String>,
}

impl StatusCache {
    const MAX: usize = 2_000;

    fn get(&self, root: &str) -> Option<u64> {
        self.map.get(root).copied()
    }

    fn insert(&mut self, root: String, signature: u64) {
        if !self.map.contains_key(&root) {
            self.order.push_back(root.clone());
            if self.order.len() > Self::MAX {
                if let Some(evicted) = self.order.pop_front() {
                    self.map.remove(&evicted);
                }
            }
        }
        self.map.insert(root, signature);
    }

    fn clear(&mut self) {
        self.map.clear();
        self.order.clear();
        self.map.shrink_to_fit();
        self.order.shrink_to_fit();
    }
}

static STATUS_CACHE: Lazy<Mutex<StatusCache>> = Lazy::new(|| {
    Mutex::new(StatusCache {
        map: HashMap::new(),
        order: VecDeque::new(),
    })
});

fn normalize(path: &str) -> String {
    path.replace('\\', "/")
}

fn start_dir(path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent().unwrap_or(path).to_path_buf()
    }
}

fn discover_repo(path: &str) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(start_dir(path))
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if root.is_empty() {
        None
    } else {
        Some(normalize(&root))
    }
}

fn parse_branch(header: &str) -> (String, i64, i64, bool) {
    fn parse_counter(header: &str, label: &str) -> i64 {
        header
            .split(label)
            .nth(1)
            .and_then(|tail| {
                let digits: String = tail
                    .chars()
                    .skip_while(|ch| !ch.is_ascii_digit())
                    .take_while(|ch| ch.is_ascii_digit())
                    .collect();
                if digits.is_empty() {
                    None
                } else {
                    digits.parse::<i64>().ok()
                }
            })
            .unwrap_or(0)
    }

    let mut branch = header.to_string();
    let ahead = parse_counter(header, "ahead");
    let behind = parse_counter(header, "behind");
    let detached = header.starts_with("HEAD");
    branch = branch
        .split(" [")
        .next()
        .unwrap_or(&branch)
        .split("...")
        .next()
        .unwrap_or(&branch)
        .to_string();
    if branch == "HEAD (no branch)" || branch.starts_with("HEAD (detached") {
        branch = "detached".to_string();
    }
    let is_detached = detached || branch == "detached";
    (branch, ahead, behind, is_detached)
}

fn classify(code: &str, index: char, worktree: char) -> &'static str {
    if code == "??" {
        "untracked"
    } else if index == 'U' || worktree == 'U' {
        "conflict"
    } else if index != ' ' && index != '?' {
        "staged"
    } else if worktree != ' ' {
        "changed"
    } else {
        "unknown"
    }
}

fn status_table(lua: &Lua, root: &str) -> LuaResult<LuaTable> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--branch", "--porcelain=v1"])
        .output()
        .map_err(|e| LuaError::RuntimeError(e.to_string()))?;
    let repo = lua.create_table()?;
    repo.set("root", root)?;
    repo.set("branch", "")?;
    repo.set("ahead", 0)?;
    repo.set("behind", 0)?;
    repo.set("detached", false)?;
    repo.set("dirty", false)?;
    repo.set("error", LuaValue::Nil)?;
    let files = lua.create_table()?;
    let ordered = lua.create_table()?;
    repo.set("files", files.clone())?;
    repo.set("ordered", ordered.clone())?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        repo.set(
            "error",
            if err.is_empty() {
                "git status failed"
            } else {
                &err
            },
        )?;
        return Ok(repo);
    }

    let mut entries = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some(head) = line.strip_prefix("## ") {
            let (branch, ahead, behind, detached) = parse_branch(head);
            repo.set("branch", branch)?;
            repo.set("ahead", ahead)?;
            repo.set("behind", behind)?;
            repo.set("detached", detached)?;
        } else if !line.starts_with("!!") && line.len() >= 4 {
            let mut rel = line[3..].to_string();
            let mut old_rel = LuaValue::Nil;
            if let Some((old, new)) = rel.split_once(" -> ") {
                old_rel = LuaValue::String(lua.create_string(old)?);
                rel = new.to_string();
            }
            let abs = normalize(&format!("{root}/{rel}"));
            let index = line.chars().next().unwrap_or(' ');
            let worktree = line.chars().nth(1).unwrap_or(' ');
            let code = &line[0..2];
            let kind = classify(code, index, worktree);
            let entry = lua.create_table()?;
            entry.set("root", root)?;
            entry.set("rel", rel.as_str())?;
            entry.set("path", abs.as_str())?;
            entry.set("old_rel", old_rel)?;
            entry.set("index", index.to_string())?;
            entry.set("worktree", worktree.to_string())?;
            entry.set("code", code)?;
            entry.set("kind", kind)?;
            files.set(abs.as_str(), entry.clone())?;
            entries.push((kind.to_string(), rel, entry));
        }
    }
    entries.sort_by(|a, b| {
        if a.0 != b.0 {
            a.0.cmp(&b.0)
        } else {
            a.1.cmp(&b.1)
        }
    });
    repo.set("dirty", !entries.is_empty())?;
    for (idx, (_, _, entry)) in entries.into_iter().enumerate() {
        ordered.raw_set((idx + 1) as i64, entry)?;
    }
    Ok(repo)
}

fn status_signature(status: i32, stdout: &[u8], stderr: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    status.hash(&mut hasher);
    stdout.hash(&mut hasher);
    stderr.hash(&mut hasher);
    hasher.finish()
}

fn status_cached(lua: &Lua, root: &str) -> LuaResult<LuaTable> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--branch", "--porcelain=v1"])
        .output()
        .map_err(|e| LuaError::RuntimeError(e.to_string()))?;
    let signature = status_signature(out.status.code().unwrap_or(-1), &out.stdout, &out.stderr);
    let changed = {
        let mut cache = STATUS_CACHE.lock();
        let changed = cache.get(root) != Some(signature);
        cache.insert(root.to_string(), signature);
        changed
    };
    let repo = status_table(lua, root)?;
    repo.set("changed", changed)?;
    repo.set("signature", signature as i64)?;
    Ok(repo)
}

fn list_branches(lua: &Lua, root: &str) -> LuaResult<LuaTable> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["branch", "--all", "--format=%(refname:short)"])
        .output()
        .map_err(|e| LuaError::RuntimeError(e.to_string()))?;
    if !out.status.success() {
        return Err(LuaError::RuntimeError(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    let mut branches = HashSet::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if !line.trim().is_empty() {
            branches.insert(line.trim().to_string());
        }
    }
    let mut list: Vec<_> = branches.into_iter().collect();
    list.sort();
    let table = lua.create_table()?;
    for (idx, branch) in list.into_iter().enumerate() {
        table.raw_set((idx + 1) as i64, branch)?;
    }
    Ok(table)
}

pub fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    let module = lua.create_table()?;
    module.set(
        "discover",
        lua.create_function(|lua, path: String| match discover_repo(&path) {
            Some(root) => Ok(LuaValue::String(lua.create_string(&root)?)),
            None => Ok(LuaValue::Nil),
        })?,
    )?;
    module.set(
        "status",
        lua.create_function(|lua, path: String| {
            let root = discover_repo(&path)
                .ok_or_else(|| LuaError::RuntimeError("Not inside a Git repository".to_string()))?;
            status_table(lua, &root)
        })?,
    )?;
    module.set(
        "status_cached",
        lua.create_function(|lua, path: String| {
            let root = discover_repo(&path)
                .ok_or_else(|| LuaError::RuntimeError("Not inside a Git repository".to_string()))?;
            status_cached(lua, &root)
        })?,
    )?;
    module.set(
        "list_branches",
        lua.create_function(|lua, path: String| {
            let root = discover_repo(&path)
                .ok_or_else(|| LuaError::RuntimeError("Not inside a Git repository".to_string()))?;
            list_branches(lua, &root)
        })?,
    )?;
    module.set(
        "clear_cache",
        lua.create_function(|_, ()| {
            STATUS_CACHE.lock().clear();
            Ok(true)
        })?,
    )?;
    Ok(module)
}

#[cfg(test)]
mod tests {
    use super::{classify, parse_branch};

    #[test]
    fn parse_branch_handles_tracking_counters() {
        let (branch, ahead, behind, detached) =
            parse_branch("main...origin/main [ahead 2, behind 1]");
        assert_eq!(branch, "main");
        assert_eq!(ahead, 2);
        assert_eq!(behind, 1);
        assert!(!detached);
    }

    #[test]
    fn parse_branch_handles_detached_head() {
        let (branch, ahead, behind, detached) = parse_branch("HEAD (detached at 1234567)");
        assert_eq!(branch, "detached");
        assert_eq!(ahead, 0);
        assert_eq!(behind, 0);
        assert!(detached);
    }

    #[test]
    fn classify_distinguishes_untracked_conflict_staged_and_changed() {
        assert_eq!(classify("??", '?', '?'), "untracked");
        assert_eq!(classify("UU", 'U', 'U'), "conflict");
        assert_eq!(classify("M ", 'M', ' '), "staged");
        assert_eq!(classify(" M", ' ', 'M'), "changed");
        assert_eq!(classify("  ", ' ', ' '), "unknown");
    }
}
