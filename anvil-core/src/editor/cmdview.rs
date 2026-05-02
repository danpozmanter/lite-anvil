//! Command-view picker: the floating text input + suggestion list used
//! for Open File / Save As / Open Folder / Open Recent. Factored out of
//! the event loop so the path-munging, recent-list management, and
//! display-string helpers live next to each other and can be tested on
//! their own. The picker's mutable state (input text, caret, selected
//! index, etc.) still lives in `main_loop::run` for now — this module
//! exposes the pure helpers that operate on it.

use std::path::{Path, PathBuf};

use crate::editor::storage;

/// Which flavour of picker is currently open. `OpenFile` and `SaveAs`
/// both show files + dirs; `OpenFolder` filters to dirs only;
/// `OpenRecent` shows a substring-filtered combined recent list;
/// `Rename` renames the file stashed in `rename_source`.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum CmdViewMode {
    OpenFile,
    OpenFolder,
    OpenRecent,
    SaveAs,
    Rename,
}

/// Shrink `text` from the LEFT until it fits inside `max_w` pixels,
/// prefixing the result with `…` when truncated. Used for the file
/// picker's suggestion list so the filename (the meaningful tail of a
/// long path) stays visible on screen instead of the drive prefix.
#[cfg(feature = "sdl")]
pub(crate) fn truncate_left_to_width(
    text: &str,
    max_w: f64,
    font: u64,
    ctx: &mut crate::editor::draw_context::NativeDrawContext,
) -> String {
    use crate::editor::view::DrawContext as _;
    if max_w <= 0.0 {
        return String::new();
    }
    if ctx.font_width(font, text) <= max_w {
        return text.to_string();
    }
    let ellipsis = "…";
    let ellipsis_w = ctx.font_width(font, ellipsis);
    let budget = (max_w - ellipsis_w).max(0.0);
    let chars: Vec<char> = text.chars().collect();
    let mut keep_from = chars.len();
    let mut w = 0.0f64;
    let mut tmp = [0u8; 4];
    for ch in chars.iter().rev() {
        let cw = ctx.font_width(font, ch.encode_utf8(&mut tmp));
        if w + cw > budget {
            break;
        }
        w += cw;
        keep_from -= 1;
    }
    if keep_from == 0 {
        text.to_string()
    } else {
        let suffix: String = chars[keep_from..].iter().collect();
        format!("{ellipsis}{suffix}")
    }
}

/// Normalise a directory path for display in the command view: strip
/// any trailing `/` or `\` and append the platform's native separator.
/// Keeps subsequent typing and the suggestions list from visually
/// mixing `/` and `\` on Windows.
pub(crate) fn dir_with_trailing_sep(path: &str) -> String {
    let sep = std::path::MAIN_SEPARATOR;
    let trimmed = path.trim_end_matches(['/', '\\']);
    format!("{trimmed}{sep}")
}

/// Absolute project root, falling back to the user's home directory if
/// empty. Windows uses `USERPROFILE`; Unix uses `HOME`.
pub(crate) fn effective_root(project_root: &str) -> String {
    if project_root.is_empty() {
        let key = if cfg!(target_os = "windows") {
            "USERPROFILE"
        } else {
            "HOME"
        };
        std::env::var(key).unwrap_or_else(|_| ".".to_string())
    } else {
        std::path::absolute(project_root)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| project_root.to_string())
    }
}

/// Add a canonicalized `path` to the recent list (dedup + prepend) and
/// cap at `limit` entries.
pub(crate) fn update_recent(list: &mut Vec<String>, path: &str, limit: usize) {
    let canonical = std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| path.to_string());
    if canonical.is_empty() {
        return;
    }
    list.retain(|p| p != &canonical);
    list.insert(0, canonical);
    if list.len() > limit {
        list.truncate(limit);
    }
}

/// Add a path to `recent_files` and persist the whole list to disk so
/// it survives an unclean shutdown.
pub(crate) fn remember_recent_file(list: &mut Vec<String>, path: &str, userdir_path: &Path) {
    update_recent(list, path, 100);
    let _ = storage::save_text(
        userdir_path,
        "session",
        "recent_files",
        &serde_json::to_string(list).unwrap_or_default(),
    );
}

/// Refresh suggestions for the current cmdview mode after the input
/// text changes. In `OpenRecent` mode, suggestions are a substring
/// filter over the combined recent-files and recent-projects lists;
/// otherwise they come from the filesystem via `path_suggest`.
pub(crate) fn refresh_cmdview_suggestions(
    mode: CmdViewMode,
    text: &str,
    project_root: &str,
    recent_files: &[String],
    recent_projects: &[String],
    include_projects: bool,
    out: &mut Vec<String>,
) {
    let dirs_only = mode == CmdViewMode::OpenFolder;
    if mode == CmdViewMode::OpenRecent {
        let query = text.to_lowercase();
        let mut combined: Vec<String> = Vec::new();
        if include_projects {
            for p in recent_projects {
                if !combined.contains(p) {
                    combined.push(p.clone());
                }
            }
        }
        for p in recent_files {
            if !combined.contains(p) {
                combined.push(p.clone());
            }
        }
        *out = if query.is_empty() {
            combined
        } else {
            combined
                .into_iter()
                .filter(|p| p.to_lowercase().contains(&query))
                .collect()
        };
    } else {
        *out = path_suggest(text, project_root, dirs_only);
    }
}

/// List filesystem entries matching a typed path prefix.
///
/// On Windows, both `/` and `\` are accepted as separators since the
/// initial `cmdview_text` and anything the user types in Explorer use
/// backslashes, while URLs / config files use forward slashes. The
/// suggestions are rendered with the platform's native separator so
/// the display stays consistent.
pub(crate) fn path_suggest(text: &str, project_root: &str, dirs_only: bool) -> Vec<String> {
    /// Cap the result set so `read_dir` + per-entry `file_type()` + sorting
    /// stays snappy even when the user points cmdview at a directory with
    /// thousands of entries (`/usr/bin`, big `node_modules`, etc.). 500 is
    /// well more than fits in the visible suggestion list and keeps each
    /// keystroke under a render frame on sluggish disks.
    const MAX_SUGGESTIONS: usize = 500;

    let sep = std::path::MAIN_SEPARATOR;
    let home_key = if cfg!(target_os = "windows") {
        "USERPROFILE"
    } else {
        "HOME"
    };

    let expanded = if let Some(rest) = text.strip_prefix('~') {
        if let Some(home) = std::env::var_os(home_key) {
            format!("{}{rest}", home.to_string_lossy())
        } else {
            text.to_string()
        }
    } else {
        text.to_string()
    };

    let last_sep = expanded.rfind(['/', '\\']);
    let (dir, prefix) = match last_sep {
        Some(pos) => (&expanded[..=pos], &expanded[pos + 1..]),
        None => (project_root, expanded.as_str()),
    };

    let lookup: PathBuf = if Path::new(dir).is_absolute() {
        PathBuf::from(dir)
    } else {
        Path::new(project_root).join(dir)
    };

    let mut results = Vec::new();
    let Ok(entries) = std::fs::read_dir(&lookup) else {
        return results;
    };
    let prefix_lower = prefix.to_lowercase();
    // Filter before sorting so the sort cost is O(matches·log(matches))
    // instead of O(all·log(all)). For a directory with thousands of files
    // and a specific typed prefix this is the difference between a snappy
    // autocomplete and a noticeable stutter on every keystroke.
    let mut matches: Vec<std::fs::DirEntry> = Vec::new();
    for entry in entries.filter_map(|e| e.ok()) {
        let name_os = entry.file_name();
        let name = name_os.to_string_lossy();
        if name.starts_with('.') && !prefix.starts_with('.') {
            continue;
        }
        if !prefix_lower.is_empty() && !name.to_lowercase().starts_with(&prefix_lower) {
            continue;
        }
        matches.push(entry);
        if matches.len() >= MAX_SUGGESTIONS * 2 {
            break;
        }
    }
    matches.sort_by_key(|e| e.file_name());

    let dir_has_trailing_sep = dir.ends_with('/') || dir.ends_with('\\');
    for entry in matches {
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if dirs_only && !is_dir {
            continue;
        }
        let display = if dir_has_trailing_sep || dir.is_empty() {
            format!("{dir}{name}")
        } else {
            format!("{dir}{sep}{name}")
        };
        let display = if is_dir {
            format!("{display}{sep}")
        } else {
            display
        };
        results.push(display);
        if results.len() >= MAX_SUGGESTIONS {
            break;
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_with_trailing_sep_strips_both_slashes() {
        let s = dir_with_trailing_sep("/tmp/foo/");
        assert!(s.starts_with("/tmp/foo"));
        assert!(s.ends_with(std::path::MAIN_SEPARATOR));
    }

    #[test]
    fn update_recent_dedups_and_caps() {
        let mut list = vec!["a".into(), "b".into(), "c".into()];
        update_recent(&mut list, "b", 5);
        assert_eq!(list[0], "b");
        assert_eq!(list.len(), 3);
    }

    #[test]
    fn update_recent_truncates_to_limit() {
        let mut list = vec!["a".into(), "b".into(), "c".into()];
        update_recent(&mut list, "d", 2);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0], "d");
    }

    #[test]
    fn refresh_open_recent_filters_by_substring() {
        let mut out = Vec::new();
        let recent_files = vec!["/home/x/foo.rs".to_string(), "/home/x/bar.rs".to_string()];
        let recent_projects = vec!["/home/x/proj".to_string()];
        refresh_cmdview_suggestions(
            CmdViewMode::OpenRecent,
            "foo",
            "",
            &recent_files,
            &recent_projects,
            true,
            &mut out,
        );
        assert_eq!(out, vec!["/home/x/foo.rs".to_string()]);
    }

    #[test]
    fn refresh_open_recent_empty_query_returns_all_with_projects() {
        let mut out = Vec::new();
        let recent_files = vec!["a".to_string(), "b".to_string()];
        let recent_projects = vec!["c".to_string()];
        refresh_cmdview_suggestions(
            CmdViewMode::OpenRecent,
            "",
            "",
            &recent_files,
            &recent_projects,
            true,
            &mut out,
        );
        assert_eq!(out.len(), 3);
        // Projects come first.
        assert_eq!(out[0], "c");
    }

    #[test]
    fn refresh_open_recent_hides_projects_when_flag_false() {
        let mut out = Vec::new();
        let recent_files = vec!["a".to_string()];
        let recent_projects = vec!["c".to_string()];
        refresh_cmdview_suggestions(
            CmdViewMode::OpenRecent,
            "",
            "",
            &recent_files,
            &recent_projects,
            false,
            &mut out,
        );
        assert_eq!(out, vec!["a".to_string()]);
    }
}
