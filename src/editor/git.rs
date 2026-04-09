use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

// ── Status cache ─────────────────────────────────────────────────────────────

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

// ── Types ────────────────────────────────────────────────────────────────────

/// A single file entry from `git status --porcelain`.
#[derive(Clone, Debug)]
pub struct FileEntry {
    pub root: String,
    pub rel: String,
    pub path: String,
    pub old_rel: Option<String>,
    pub index: String,
    pub worktree: String,
    pub code: String,
    pub kind: &'static str,
}

/// Mutable per-repo state for the async refresh system.
pub struct RepoState {
    pub branch: String,
    pub ahead: i64,
    pub behind: i64,
    pub detached: bool,
    pub dirty: bool,
    pub refreshing: bool,
    pub last_refresh: f64,
    pub error: Option<String>,
    pub ordered: Vec<FileEntry>,
    pub files_by_path: HashMap<String, usize>,
}

impl Default for RepoState {
    fn default() -> Self {
        Self {
            branch: String::new(),
            ahead: 0,
            behind: 0,
            detached: false,
            dirty: false,
            refreshing: false,
            last_refresh: 0.0,
            error: None,
            ordered: Vec::new(),
            files_by_path: HashMap::new(),
        }
    }
}

/// Result of a background refresh.
pub enum RefreshOutcome {
    Success {
        branch: String,
        ahead: i64,
        behind: i64,
        detached: bool,
        ordered: Vec<FileEntry>,
    },
    Failure(String),
}

/// Result of a git command.
pub struct CommandResult {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
}

// ── Global state ─────────────────────────────────────────────────────────────

pub static REPOS: Lazy<Mutex<HashMap<String, RepoState>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
pub static PATH_ROOTS: Lazy<Mutex<HashMap<String, Option<String>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
pub static PENDING: Lazy<Mutex<VecDeque<(String, RefreshOutcome)>>> =
    Lazy::new(|| Mutex::new(VecDeque::new()));
pub static COMMANDS: Lazy<Mutex<HashMap<u64, Option<CommandResult>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
pub static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

// ── Pure functions ───────────────────────────────────────────────────────────

/// Normalize path separators to forward slashes.
pub fn normalize(path: &str) -> String {
    path.replace('\\', "/")
}

/// Monotonic seconds since first call.
pub fn monotonic_secs() -> f64 {
    static START: Lazy<std::time::Instant> = Lazy::new(std::time::Instant::now);
    START.elapsed().as_secs_f64()
}

/// Starting directory for git discovery (the dir itself if dir, else parent).
pub fn start_dir(path: &str) -> PathBuf {
    let path = Path::new(path);
    if path.is_dir() {
        path.to_path_buf()
    } else {
        path.parent().unwrap_or(path).to_path_buf()
    }
}

/// Discover the git repo root for a path.
pub fn discover_repo(path: &str) -> Option<String> {
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

/// Cached root discovery.
pub fn get_or_discover_root(path: &str) -> Option<String> {
    if path.is_empty() {
        return None;
    }
    let key = normalize(&start_dir(path).to_string_lossy());
    {
        let cache = PATH_ROOTS.lock();
        if let Some(cached) = cache.get(&key) {
            return cached.clone();
        }
    }
    let root = discover_repo(path);
    PATH_ROOTS.lock().insert(key, root.clone());
    root
}

/// Parse the branch header from `git status --porcelain`.
pub fn parse_branch(header: &str) -> (String, i64, i64, bool) {
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

/// Classify a status code into a kind string.
pub fn classify(code: &str, index: char, worktree: char) -> &'static str {
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

/// Parse `git status --porcelain=v1` output into a `RefreshOutcome`.
pub fn parse_status_raw(root: &str, stdout: &str, stderr: &str, success: bool) -> RefreshOutcome {
    if !success {
        return RefreshOutcome::Failure(if stderr.trim().is_empty() {
            "git status failed".to_string()
        } else {
            stderr.trim().to_string()
        });
    }
    let mut branch = String::new();
    let mut ahead = 0i64;
    let mut behind = 0i64;
    let mut detached = false;
    let mut entries: Vec<FileEntry> = Vec::new();
    for line in stdout.lines() {
        if let Some(head) = line.strip_prefix("## ") {
            let (b, a, be, d) = parse_branch(head);
            branch = b;
            ahead = a;
            behind = be;
            detached = d;
        } else if !line.starts_with("!!") && line.len() >= 4 {
            let mut rel = line[3..].to_string();
            let mut old_rel: Option<String> = None;
            if let Some((old, new)) = rel.split_once(" -> ") {
                old_rel = Some(old.to_string());
                rel = new.to_string();
            }
            let abs = normalize(&format!("{root}/{rel}"));
            let index = line.chars().next().unwrap_or(' ');
            let worktree = line.chars().nth(1).unwrap_or(' ');
            let code = line[0..2].to_string();
            let kind = classify(&code, index, worktree);
            entries.push(FileEntry {
                root: root.to_string(),
                rel,
                path: abs,
                old_rel,
                index: index.to_string(),
                worktree: worktree.to_string(),
                code,
                kind,
            });
        }
    }
    entries.sort_by(|a, b| {
        if a.kind != b.kind {
            a.kind.cmp(b.kind)
        } else {
            a.rel.cmp(&b.rel)
        }
    });
    RefreshOutcome::Success {
        branch,
        ahead,
        behind,
        detached,
        ordered: entries,
    }
}

/// Hash the git status output for change detection.
pub fn status_signature(status: i32, stdout: &[u8], stderr: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    status.hash(&mut hasher);
    stdout.hash(&mut hasher);
    stderr.hash(&mut hasher);
    hasher.finish()
}

/// Spawn a background refresh thread for `root`.
pub fn start_refresh_if_idle(root: String) {
    let should_start = {
        let mut repos = REPOS.lock();
        let state = repos.entry(root.clone()).or_default();
        if state.refreshing {
            false
        } else {
            state.refreshing = true;
            true
        }
    };
    if !should_start {
        return;
    }
    std::thread::spawn(move || {
        let out = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["status", "--branch", "--porcelain=v1"])
            .output();
        let outcome = match out {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout).to_string();
                let stderr = String::from_utf8_lossy(&o.stderr).to_string();
                parse_status_raw(&root, &stdout, &stderr, o.status.success())
            }
            Err(e) => RefreshOutcome::Failure(e.to_string()),
        };
        PENDING.lock().push_back((root, outcome));
    });
}

/// Apply pending refresh outcomes to repo state.
pub fn apply_pending_updates() -> Vec<String> {
    let items: Vec<(String, RefreshOutcome)> = PENDING.lock().drain(..).collect();
    if items.is_empty() {
        return Vec::new();
    }
    let updated: Vec<String> = items.iter().map(|(r, _)| r.clone()).collect();
    let mut repos = REPOS.lock();
    for (root, outcome) in items {
        let s = repos.entry(root).or_default();
        s.refreshing = false;
        s.last_refresh = monotonic_secs();
        match outcome {
            RefreshOutcome::Success {
                branch,
                ahead,
                behind,
                detached,
                ordered,
            } => {
                s.files_by_path.clear();
                for (i, e) in ordered.iter().enumerate() {
                    s.files_by_path.insert(e.path.clone(), i);
                }
                s.ordered = ordered;
                s.branch = branch;
                s.ahead = ahead;
                s.behind = behind;
                s.detached = detached;
                s.dirty = !s.ordered.is_empty();
                s.error = None;
            }
            RefreshOutcome::Failure(err) => {
                s.error = Some(err);
            }
        }
    }
    updated
}

/// Start a git command asynchronously. Returns a handle for polling.
pub fn start_command(root: &str, args: &[String]) -> u64 {
    let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    COMMANDS.lock().insert(handle, None);
    let root = root.to_string();
    let args: Vec<String> = args.to_vec();
    std::thread::spawn(move || {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&root);
        for arg in &args {
            cmd.arg(arg);
        }
        let result = match cmd.output() {
            Ok(o) => CommandResult {
                ok: o.status.success(),
                stdout: String::from_utf8_lossy(&o.stdout).to_string(),
                stderr: String::from_utf8_lossy(&o.stderr).trim().to_string(),
            },
            Err(e) => CommandResult {
                ok: false,
                stdout: String::new(),
                stderr: e.to_string(),
            },
        };
        if let Some(slot) = COMMANDS.lock().get_mut(&handle) {
            *slot = Some(result);
        }
    });
    handle
}

/// Check if a command has completed. Returns the result and removes it.
pub fn check_command(handle: u64) -> Option<CommandResult> {
    let mut commands = COMMANDS.lock();
    match commands.get(&handle) {
        None | Some(None) => None,
        Some(Some(_)) => {
            let Some(Some(val)) = commands.remove(&handle) else {
                unreachable!()
            };
            Some(val)
        }
    }
}

/// List branches for a repo root.
pub fn list_branches(root: &str) -> Result<Vec<String>, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["branch", "--all", "--format=%(refname:short)"])
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let mut branches = HashSet::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if !line.trim().is_empty() {
            branches.insert(line.trim().to_string());
        }
    }
    let mut list: Vec<_> = branches.into_iter().collect();
    list.sort();
    Ok(list)
}

/// Clear all caches.
pub fn clear_cache() {
    STATUS_CACHE.lock().clear();
    PATH_ROOTS.lock().clear();
}

/// Line-level git change type for gutter markers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineChange {
    Added,
    Modified,
    Deleted,
}

/// Returns a map of 1-based line numbers to their change type by running `git diff` on the file.
pub fn diff_file(file_path: &str) -> HashMap<usize, LineChange> {
    let mut result = HashMap::new();
    let out = match Command::new("git")
        .args(["diff", "--unified=0", "--no-color", "--"])
        .arg(file_path)
        .output()
    {
        Ok(o) => o,
        Err(_) => return result,
    };
    if !out.status.success() {
        return result;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    for line in stdout.lines() {
        if !line.starts_with("@@") {
            continue;
        }
        // Parse unified diff hunk header: @@ -old_start[,old_count] +new_start[,new_count] @@
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 3 {
            continue;
        }
        let old_part = parts[1].trim_start_matches('-');
        let new_part = parts[2].trim_start_matches('+');
        let (old_count, _old_start) = parse_hunk_range(old_part);
        let (new_count, new_start) = parse_hunk_range(new_part);
        if old_count == 0 {
            // Pure addition
            for ln in new_start..new_start + new_count {
                result.insert(ln, LineChange::Added);
            }
        } else if new_count == 0 {
            // Pure deletion — mark the line after the deletion point
            result.insert(new_start.max(1), LineChange::Deleted);
        } else {
            // Modification
            for ln in new_start..new_start + new_count {
                result.insert(ln, LineChange::Modified);
            }
        }
    }
    result
}

fn parse_hunk_range(s: &str) -> (usize, usize) {
    if let Some((start, count)) = s.split_once(',') {
        (
            count.parse().unwrap_or(1),
            start.parse().unwrap_or(1),
        )
    } else {
        (1, s.parse().unwrap_or(1))
    }
}

/// Get cached status signature for change detection.
pub fn get_cached_signature(root: &str) -> Option<u64> {
    STATUS_CACHE.lock().get(root)
}

/// Insert a status signature.
pub fn insert_cached_signature(root: &str, signature: u64) {
    STATUS_CACHE.lock().insert(root.to_string(), signature);
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn classify_distinguishes_status_kinds() {
        assert_eq!(classify("??", '?', '?'), "untracked");
        assert_eq!(classify("UU", 'U', 'U'), "conflict");
        assert_eq!(classify("M ", 'M', ' '), "staged");
        assert_eq!(classify(" M", ' ', 'M'), "changed");
        assert_eq!(classify("  ", ' ', ' '), "unknown");
    }

    #[test]
    fn parse_status_raw_failure() {
        let outcome = parse_status_raw("/root", "", "fatal: error", false);
        assert!(matches!(outcome, RefreshOutcome::Failure(_)));
    }

    #[test]
    fn parse_status_raw_success() {
        let stdout = "## main...origin/main\n M src/main.rs\n?? new.txt\n";
        let outcome = parse_status_raw("/root", stdout, "", true);
        match outcome {
            RefreshOutcome::Success {
                branch, ordered, ..
            } => {
                assert_eq!(branch, "main");
                assert_eq!(ordered.len(), 2);
            }
            RefreshOutcome::Failure(_) => panic!("expected success"),
        }
    }

    #[test]
    fn status_signature_deterministic() {
        let s1 = status_signature(0, b"abc", b"");
        let s2 = status_signature(0, b"abc", b"");
        assert_eq!(s1, s2);
    }

    #[test]
    fn status_signature_changes_on_different_input() {
        let s1 = status_signature(0, b"abc", b"");
        let s2 = status_signature(0, b"def", b"");
        assert_ne!(s1, s2);
    }

    #[test]
    fn normalize_replaces_backslashes() {
        assert_eq!(normalize("C:\\foo\\bar"), "C:/foo/bar");
    }
}
