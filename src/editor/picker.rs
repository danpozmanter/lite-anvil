use std::collections::HashSet;

/// Normalize a needle string for platform-specific path matching.
pub fn normalize_needle(needle: &str, files: bool) -> String {
    if cfg!(windows) && files {
        needle.replace('/', "\\")
    } else {
        needle.to_string()
    }
}

/// Rank strings by fuzzy match score, with optional recents-first ordering.
pub fn rank_strings(
    items: Vec<String>,
    needle: &str,
    files: bool,
    recents: &[String],
    limit: Option<usize>,
) -> Vec<String> {
    let needle = normalize_needle(needle, files);
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    if needle.is_empty() {
        for recent in recents {
            if seen.insert(recent.clone()) {
                out.push(recent.clone());
            }
        }
    }

    let mut ranked: Vec<(String, i64)> = items
        .into_iter()
        .filter_map(|item| {
            crate::editor::common::fuzzy_match(&item, &needle, files).map(|score| (item, score))
        })
        .collect();
    ranked.sort_by(|a, b| match b.1.cmp(&a.1) {
        std::cmp::Ordering::Equal => a.0.cmp(&b.0),
        other => other,
    });

    for (item, _) in ranked {
        if seen.insert(item.clone()) {
            out.push(item);
            if let Some(limit) = limit {
                if out.len() >= limit {
                    break;
                }
            }
        }
    }

    out
}

// ── Affordance model ─────────────────────────────────────────────────────────

/// Indentation level of a line (tabs count as 4 spaces).
pub fn indent_of(text: &str) -> usize {
    text.chars()
        .take_while(|ch| *ch == ' ' || *ch == '\t')
        .map(|ch| if ch == '\t' { 4 } else { 1 })
        .sum()
}

/// Find the last line of a foldable block starting at `line` (1-based).
pub fn get_fold_end(lines: &[String], line: usize) -> Option<usize> {
    let idx = line.checked_sub(1)?;
    let line_text = lines.get(idx)?;
    if line_text.trim().is_empty() {
        return None;
    }
    let base = indent_of(line_text);
    let mut next_indent = None;
    let mut end_line = None;
    for (offset, text) in lines.iter().enumerate().skip(line) {
        if text.trim().is_empty() {
            continue;
        }
        let indent = indent_of(text);
        if next_indent.is_none() {
            if indent <= base {
                return None;
            }
            next_indent = Some(indent);
        } else if indent <= base {
            return end_line;
        }
        end_line = Some(offset + 1);
    }
    end_line
}

/// Count visible (non-folded) lines.
pub fn visible_line_count(line_count: usize, folds: &[(usize, usize)]) -> usize {
    line_count.saturating_sub(
        folds
            .iter()
            .map(|(start, end_line)| end_line.saturating_sub(*start))
            .sum::<usize>(),
    )
}

/// Convert an actual line number to its visible position, accounting for folds.
pub fn actual_to_visible(line: usize, folds: &[(usize, usize)]) -> usize {
    let mut visible = line;
    for (start, end_line) in folds {
        if line > *end_line {
            visible = visible.saturating_sub(end_line - start);
        } else if line > *start {
            visible = visible.saturating_sub(line - start);
        }
    }
    visible.max(1)
}

/// Convert a visible line position to its actual line number.
pub fn visible_to_actual(visible: usize, line_count: usize, folds: &[(usize, usize)]) -> usize {
    let mut actual = 1usize;
    let mut seen = 0usize;
    while actual <= line_count {
        let mut hidden = None;
        for (start, end_line) in folds {
            if actual > *start && actual <= *end_line {
                hidden = Some(*end_line);
                break;
            }
        }
        if hidden.is_none() {
            seen += 1;
            if seen >= visible {
                return actual;
            }
        }
        actual = hidden.map(|end_line| end_line + 1).unwrap_or(actual + 1);
    }
    line_count.max(1)
}

/// Next visible line after `line`, skipping folded regions.
pub fn next_visible_line(line: usize, folds: &[(usize, usize)]) -> usize {
    for (start, end_line) in folds {
        if line >= *start && line < *end_line {
            return end_line + 1;
        }
    }
    line + 1
}

/// Find matching bracket pair from a position (1-based).
pub fn bracket_pair(
    lines: &[String],
    start_line: usize,
    start_col: usize,
) -> Option<(usize, usize, usize, usize)> {
    const LIMIT: usize = 2000;
    let line_idx = start_line.checked_sub(1)?;
    let text = lines.get(line_idx)?;
    let chars: Vec<char> = text.chars().collect();
    let col_idx = start_col.checked_sub(1)?;
    let ch = *chars.get(col_idx)?;
    let (open, close, dir) = match ch {
        '(' => ('(', ')', 1isize),
        ')' => ('(', ')', -1isize),
        '[' => ('[', ']', 1),
        ']' => ('[', ']', -1),
        '{' => ('{', '}', 1),
        '}' => ('{', '}', -1),
        _ => return None,
    };
    let mut depth = 1isize;
    if dir > 0 {
        let end = (start_line + LIMIT).min(lines.len());
        for line in start_line..=end {
            let chars: Vec<char> = lines[line - 1].chars().collect();
            let start = if line == start_line { start_col + 1 } else { 1 };
            for col in start..=chars.len() {
                let cur = chars[col - 1];
                if cur == open {
                    depth += 1;
                } else if cur == close {
                    depth -= 1;
                    if depth == 0 {
                        return Some((start_line, start_col, line, col));
                    }
                }
            }
        }
    } else {
        let start = start_line.saturating_sub(LIMIT);
        for line in (start.max(1)..=start_line).rev() {
            let chars: Vec<char> = lines[line - 1].chars().collect();
            let end_col = if line == start_line {
                start_col.saturating_sub(1).min(chars.len())
            } else {
                chars.len().saturating_sub(1)
            };
            for col in (1..=end_col).rev() {
                let cur = chars[col - 1];
                if cur == close {
                    depth += 1;
                } else if cur == open {
                    depth -= 1;
                    if depth == 0 {
                        return Some((start_line, start_col, line, col));
                    }
                }
            }
        }
    }
    None
}

/// Trim trailing whitespace, but preserve up to `caret_col` characters.
pub fn trim_line(text: &str, caret_col: Option<usize>) -> String {
    let trimmed = text.trim_end_matches(char::is_whitespace);
    if let Some(caret_col) = caret_col {
        if caret_col > trimmed.chars().count() {
            return text.chars().take(caret_col.saturating_sub(1)).collect();
        }
    }
    trimmed.to_string()
}

/// Count consecutive empty lines at the end of a document.
pub fn count_empty_end_lines(lines: &[String]) -> usize {
    let mut count = 0usize;
    for line in lines.iter().rev() {
        if line == "\n" {
            count += 1;
        } else {
            break;
        }
    }
    count
}

/// Detect indentation style and size from document lines.
/// Returns (type, size, score) where type is "soft" or "hard".
pub fn detect_indent(
    lines: &[String],
    max_lines: usize,
    default_indent: usize,
) -> (&'static str, usize, usize) {
    let mut stats = Vec::new();
    let mut tabs = 0usize;
    for text in lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .take(max_lines)
    {
        let spaces = text.chars().take_while(|ch| *ch == ' ').count();
        if spaces > 1 {
            stats.push(spaces);
        }
        if text.starts_with('\t') {
            tabs += 1;
        }
    }
    stats.sort_unstable_by(|a, b| b.cmp(a));
    let mut best_indent = default_indent;
    let mut best_score = 0usize;
    for &indent in &stats {
        let score = stats
            .iter()
            .filter(|&&candidate| candidate != indent && candidate % indent == 0)
            .count();
        if score > best_score {
            best_indent = indent;
            best_score = score;
        }
    }
    if tabs > best_score {
        ("hard", default_indent, tabs)
    } else {
        ("soft", best_indent, best_score)
    }
}

/// Check if a file should trigger editor auto-restart on save.
pub fn should_autorestart(
    abs_filename: &str,
    userdir: &str,
    pathsep: &str,
    project_path: Option<&str>,
) -> bool {
    let user_init = format!("{userdir}{pathsep}init.lua");
    let user_config = format!("{userdir}{pathsep}config.lua");
    if abs_filename == user_init || abs_filename == user_config {
        return true;
    }
    if let Some(project_path) = project_path {
        let project_file = format!("{project_path}{pathsep}.lite_project");
        return abs_filename == project_file;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_strings_recents_first() {
        let ranked = rank_strings(
            vec![
                "src/main.rs".into(),
                "README.md".into(),
                "Cargo.toml".into(),
            ],
            "",
            true,
            &["README.md".into()],
            None,
        );
        assert_eq!(ranked.first().map(String::as_str), Some("README.md"));
    }

    #[test]
    fn rank_strings_matching_only() {
        let ranked = rank_strings(
            vec!["alpha".into(), "beta".into(), "gamma".into()],
            "bt",
            false,
            &[],
            None,
        );
        assert_eq!(ranked, vec!["beta"]);
    }

    #[test]
    fn folds_and_visibility() {
        let lines = vec![
            "fn main()\n".into(),
            "    let x = 1;\n".into(),
            "    let y = 2;\n".into(),
            "println!(\"hi\");\n".into(),
        ];
        assert_eq!(get_fold_end(&lines, 1), Some(3));
        let folds = vec![(1, 3)];
        assert_eq!(visible_line_count(lines.len(), &folds), 2);
        assert_eq!(actual_to_visible(3, &folds), 1);
        assert_eq!(visible_to_actual(2, lines.len(), &folds), 4);
    }

    #[test]
    fn bracket_pairs_match() {
        let lines = vec!["fn(a[0])\n".into()];
        assert_eq!(bracket_pair(&lines, 1, 3), Some((1, 3, 1, 8)));
    }

    #[test]
    fn indent_detection_prefers_tabs() {
        let lines = vec!["\tfoo\n".into(), "\tbar\n".into(), "  baz\n".into()];
        let (kind, size, score) = detect_indent(&lines, 150, 2);
        assert_eq!(kind, "hard");
        assert_eq!(size, 2);
        assert_eq!(score, 2);
    }

    #[test]
    fn trim_line_basic() {
        assert_eq!(trim_line("hello   ", None), "hello");
    }

    #[test]
    fn trim_line_with_caret() {
        assert_eq!(trim_line("hello   ", Some(7)), "hello ");
    }

    #[test]
    fn count_empty_end_lines_basic() {
        let lines = vec!["hello\n".into(), "\n".into(), "\n".into()];
        assert_eq!(count_empty_end_lines(&lines), 2);
    }

    #[test]
    fn should_autorestart_user_config() {
        assert!(should_autorestart(
            "/home/user/.config/lite-anvil/init.lua",
            "/home/user/.config/lite-anvil",
            "/",
            None,
        ));
    }
}
