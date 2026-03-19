use mlua::prelude::*;

fn lua_lines(lines: LuaTable) -> LuaResult<Vec<String>> {
    let mut out = Vec::new();
    for value in lines.sequence_values::<String>() {
        out.push(value?);
    }
    Ok(out)
}

fn lua_folds(folds: LuaTable) -> LuaResult<Vec<(usize, usize)>> {
    let mut out = Vec::new();
    for pair in folds.pairs::<LuaValue, LuaValue>() {
        let (k, v) = pair?;
        if let (LuaValue::Integer(start), LuaValue::Integer(end_line)) = (k, v)
            && start >= 1
            && end_line >= start
        {
            out.push((start as usize, end_line as usize));
        }
    }
    out.sort_unstable();
    Ok(out)
}

fn indent_of(text: &str) -> usize {
    text.chars()
        .take_while(|ch| *ch == ' ' || *ch == '\t')
        .map(|ch| if ch == '\t' { 4 } else { 1 })
        .sum()
}

fn get_fold_end(lines: &[String], line: usize) -> Option<usize> {
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

fn visible_line_count(line_count: usize, folds: &[(usize, usize)]) -> usize {
    line_count.saturating_sub(
        folds
            .iter()
            .map(|(start, end_line)| end_line.saturating_sub(*start))
            .sum::<usize>(),
    )
}

fn actual_to_visible(line: usize, folds: &[(usize, usize)]) -> usize {
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

fn visible_to_actual(visible: usize, line_count: usize, folds: &[(usize, usize)]) -> usize {
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

fn next_visible_line(line: usize, folds: &[(usize, usize)]) -> usize {
    for (start, end_line) in folds {
        if line + 1 > *start && line + 1 <= *end_line {
            return end_line + 1;
        }
    }
    line + 1
}

fn bracket_pair(
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

fn trim_line(text: &str, caret_col: Option<usize>) -> String {
    let trimmed = text.trim_end_matches(char::is_whitespace);
    if let Some(caret_col) = caret_col
        && caret_col > trimmed.chars().count()
    {
        return text.chars().take(caret_col.saturating_sub(1)).collect();
    }
    trimmed.to_string()
}

fn count_empty_end_lines(lines: &[String]) -> usize {
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

fn detect_indent(
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

fn should_autorestart(
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

pub fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    let module = lua.create_table()?;
    module.set(
        "bracket_pair",
        lua.create_function(|lua, (lines, line, col): (LuaTable, usize, usize)| {
            let lines = lua_lines(lines)?;
            if let Some((l1, c1, l2, c2)) = bracket_pair(&lines, line, col) {
                let out = lua.create_table()?;
                out.set(1, l1)?;
                out.set(2, c1)?;
                out.set(3, l2)?;
                out.set(4, c2)?;
                Ok(LuaValue::Table(out))
            } else {
                Ok(LuaValue::Nil)
            }
        })?,
    )?;
    module.set(
        "get_fold_end",
        lua.create_function(|_, (lines, line): (LuaTable, usize)| {
            Ok(get_fold_end(&lua_lines(lines)?, line).map(|value| value as i64))
        })?,
    )?;
    module.set(
        "visible_line_count",
        lua.create_function(|_, (line_count, folds): (usize, LuaTable)| {
            Ok(visible_line_count(line_count, &lua_folds(folds)?) as i64)
        })?,
    )?;
    module.set(
        "actual_to_visible",
        lua.create_function(|_, (line, folds): (usize, LuaTable)| {
            Ok(actual_to_visible(line, &lua_folds(folds)?) as i64)
        })?,
    )?;
    module.set(
        "visible_to_actual",
        lua.create_function(
            |_, (visible, line_count, folds): (usize, usize, LuaTable)| {
                Ok(visible_to_actual(visible, line_count, &lua_folds(folds)?) as i64)
            },
        )?,
    )?;
    module.set(
        "next_visible_line",
        lua.create_function(|_, (line, folds): (usize, LuaTable)| {
            Ok(next_visible_line(line, &lua_folds(folds)?) as i64)
        })?,
    )?;
    module.set(
        "trim_line",
        lua.create_function(|lua, (text, caret_col): (String, Option<usize>)| {
            Ok(LuaValue::String(
                lua.create_string(trim_line(&text, caret_col).as_bytes())?,
            ))
        })?,
    )?;
    module.set(
        "count_empty_end_lines",
        lua.create_function(|_, lines: LuaTable| {
            Ok(count_empty_end_lines(&lua_lines(lines)?) as i64)
        })?,
    )?;
    module.set(
        "detect_indent",
        lua.create_function(
            |lua, (lines, max_lines, default_indent): (LuaTable, usize, usize)| {
                let (kind, size, score) =
                    detect_indent(&lua_lines(lines)?, max_lines, default_indent);
                let out = lua.create_table()?;
                out.set("type", kind)?;
                out.set("size", size)?;
                out.set("score", score)?;
                Ok(out)
            },
        )?,
    )?;
    module.set(
        "should_autorestart",
        lua.create_function(
            |_,
             (abs_filename, userdir, pathsep, project_path): (
                String,
                String,
                String,
                Option<String>,
            )| {
                Ok(should_autorestart(
                    &abs_filename,
                    &userdir,
                    &pathsep,
                    project_path.as_deref(),
                ))
            },
        )?,
    )?;
    Ok(module)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_and_visibility_work() {
        let lines = vec![
            "fn main()\n".to_string(),
            "    let x = 1;\n".to_string(),
            "    let y = 2;\n".to_string(),
            "println!(\"hi\");\n".to_string(),
        ];
        assert_eq!(get_fold_end(&lines, 1), Some(3));
        let folds = vec![(1, 3)];
        assert_eq!(visible_line_count(lines.len(), &folds), 2);
        assert_eq!(actual_to_visible(3, &folds), 1);
        assert_eq!(visible_to_actual(2, lines.len(), &folds), 4);
    }

    #[test]
    fn bracket_pairs_match() {
        let lines = vec!["fn(a[0])\n".to_string()];
        assert_eq!(bracket_pair(&lines, 1, 3), Some((1, 3, 1, 8)));
    }

    #[test]
    fn indent_detection_prefers_tabs_when_stronger() {
        let lines = vec![
            "\tfoo\n".to_string(),
            "\tbar\n".to_string(),
            "  baz\n".to_string(),
        ];
        let (kind, size, score) = detect_indent(&lines, 150, 2);
        assert_eq!(kind, "hard");
        assert_eq!(size, 2);
        assert_eq!(score, 2);
    }
}
