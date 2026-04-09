// ── Node model (tab layout) ──────────────────────────────────────────────────

/// Number of visible tabs given view count, offset, and max.
pub fn visible_tabs(view_count: usize, tab_offset: usize, max_tabs: usize) -> usize {
    if view_count == 0 {
        return 0;
    }
    view_count
        .saturating_sub(tab_offset.saturating_sub(1))
        .min(max_tabs.max(1))
}

/// Move tab index by direction, clamping to bounds.
pub fn move_tab_index(view_count: usize, current_index: usize, direction: i64) -> usize {
    if view_count == 0 {
        return 0;
    }
    let current = current_index.clamp(1, view_count);
    match direction.cmp(&0) {
        std::cmp::Ordering::Less => current.saturating_sub(1).max(1),
        std::cmp::Ordering::Greater => current.saturating_add(1).min(view_count),
        std::cmp::Ordering::Equal => current,
    }
}

/// Move tab index with wrapping.
pub fn wrapped_tab_index(view_count: usize, current_index: usize, direction: i64) -> usize {
    if view_count == 0 {
        return 0;
    }
    let current = current_index.clamp(1, view_count);
    if direction < 0 {
        if current == 1 {
            view_count
        } else {
            current - 1
        }
    } else if direction > 0 {
        if current >= view_count {
            1
        } else {
            current + 1
        }
    } else {
        current
    }
}

/// Adjust tab offset to ensure active tab is visible.
pub fn ensure_visible_tab_offset(
    view_count: usize,
    tab_offset: usize,
    max_tabs: usize,
    active_index: usize,
) -> usize {
    if view_count == 0 {
        return 1;
    }
    let tabs_number = visible_tabs(view_count, tab_offset, max_tabs).max(1);
    let mut offset = tab_offset.clamp(1, view_count);
    let active = active_index.clamp(1, view_count);
    if offset > active {
        offset = active;
    } else if offset + tabs_number - 1 < active {
        offset = active - tabs_number + 1;
    } else if tabs_number < max_tabs.max(1) && offset > 1 {
        offset = view_count
            .saturating_sub(max_tabs.max(1))
            .saturating_add(1)
            .max(1);
    }
    offset.clamp(1, view_count)
}

/// Scroll tab offset and adjust active index.
pub fn scroll_tab_offset(
    view_count: usize,
    tab_offset: usize,
    max_tabs: usize,
    active_index: usize,
    direction: i64,
) -> (usize, usize) {
    if view_count == 0 {
        return (1, 0);
    }
    let mut offset = tab_offset.clamp(1, view_count);
    let mut active = active_index.clamp(1, view_count);
    if direction < 0 {
        if offset > 1 {
            offset -= 1;
            let last_index = offset + visible_tabs(view_count, offset, max_tabs).saturating_sub(1);
            if active > last_index {
                active = last_index.max(1);
            }
        }
    } else if direction > 0 {
        let tabs_number = visible_tabs(view_count, offset, max_tabs);
        if offset + tabs_number.saturating_sub(1) < view_count {
            offset += 1;
            if active < offset {
                active = offset;
            }
        }
    }
    (offset, active)
}

/// Calculate target tab width given constraints.
pub fn target_tab_width(
    size_x: f64,
    view_count: usize,
    tab_offset: usize,
    max_tabs: usize,
    tab_width: f64,
) -> f64 {
    let visible = visible_tabs(view_count, tab_offset, max_tabs).max(1) as f64;
    let width = size_x.max(1.0);
    let min_width = width / (max_tabs.max(1) as f64);
    let max_width = width / visible;
    tab_width.clamp(min_width, max_width)
}

/// Find which tab index a pixel x-coordinate hits.
pub fn tab_hit_index(
    view_count: usize,
    tab_offset: usize,
    max_tabs: usize,
    tab_width: f64,
    tab_shift: f64,
    max_width: f64,
    px: f64,
) -> usize {
    let visible = visible_tabs(view_count, tab_offset, max_tabs);
    if visible == 0 {
        return 0;
    }
    let x1 = (tab_width * (tab_offset.saturating_sub(1)) as f64 - tab_shift).clamp(0.0, max_width);
    let x2 = (tab_width * (tab_offset + visible - 1) as f64 - tab_shift).clamp(0.0, max_width);
    if px < x1 || px >= x2 || tab_width <= 0.0 {
        return 0;
    }
    ((px - x1) / tab_width).floor() as usize + tab_offset
}

/// Determine split direction from mouse position within a node.
pub fn split_type(size_x: f64, size_y: f64, tab_height: f64, mouse_x: f64, mouse_y: f64) -> String {
    let local_mouse_y = mouse_y - tab_height;
    let height = (size_y - tab_height).max(1.0);
    if local_mouse_y < 0.0 {
        return "tab".to_string();
    }
    let left_pct = mouse_x * 100.0 / size_x.max(1.0);
    let top_pct = local_mouse_y * 100.0 / height;
    if left_pct <= 30.0 {
        "left".to_string()
    } else if left_pct >= 70.0 {
        "right".to_string()
    } else if top_pct <= 30.0 {
        "up".to_string()
    } else if top_pct >= 70.0 {
        "down".to_string()
    } else {
        "middle".to_string()
    }
}

/// Calculate drag overlay position for tab reordering.
#[allow(clippy::too_many_arguments)]
pub fn drag_overlay_tab_position(
    view_count: usize,
    tab_offset: usize,
    max_tabs: usize,
    tab_width: f64,
    tab_shift: f64,
    max_width: f64,
    px: f64,
    dragged_index: usize,
) -> (usize, f64, f64) {
    let mut tab_index = tab_hit_index(
        view_count, tab_offset, max_tabs, tab_width, tab_shift, max_width, px,
    );
    if tab_index == 0 {
        if px < 0.0 {
            tab_index = tab_offset.max(1);
        } else {
            tab_index =
                visible_tabs(view_count, tab_offset, max_tabs) + tab_offset.saturating_sub(1);
            if tab_index == 0 {
                tab_index = 1;
            }
        }
    }
    let clamped_idx = tab_index.clamp(1, view_count.max(1));
    let tab_x =
        (tab_width * (clamped_idx.saturating_sub(1)) as f64 - tab_shift).clamp(0.0, max_width);
    let next_x = (tab_width * clamped_idx as f64 - tab_shift).clamp(0.0, max_width);
    let mut out_index = tab_index;
    let mut out_x = tab_x;
    let out_w = (next_x - tab_x).max(0.0);
    if px > tab_x + out_w / 2.0 && tab_index <= view_count {
        out_x = next_x;
        out_index += 1;
    }
    if dragged_index > 0 && out_index > dragged_index {
        out_index -= 1;
        out_x = (out_x - out_w).max(0.0);
    }
    (out_index, out_x, out_w)
}

// ── Status model (panel layout) ──────────────────────────────────────────────

/// Fitted panel dimensions.
#[derive(Clone, Copy)]
pub struct PanelFit {
    pub left_width: f64,
    pub right_width: f64,
    pub left_offset: f64,
    pub right_offset: f64,
}

/// Calculate panel widths and offsets that fit within total_width.
pub fn fit_panels(
    total_width: f64,
    raw_left: f64,
    raw_right: f64,
    padding: f64,
    current_left_offset: f64,
    current_right_offset: f64,
) -> PanelFit {
    let mut left_width = raw_left;
    let mut right_width = raw_right;
    let mut left_offset = current_left_offset;
    let mut right_offset = current_right_offset;

    if raw_left + raw_right + (padding * 4.0) > total_width {
        if raw_left + (padding * 2.0) < total_width / 2.0 {
            right_width = total_width - raw_left - (padding * 3.0);
            if right_width > raw_right {
                left_width = raw_left + (right_width - raw_right);
                right_width = raw_right;
            }
        } else if raw_right + (padding * 2.0) < total_width / 2.0 {
            left_width = total_width - raw_right - (padding * 3.0);
        } else {
            left_width = total_width / 2.0 - (padding + padding / 2.0);
            right_width = total_width / 2.0 - (padding + padding / 2.0);
        }

        if right_width >= raw_right {
            right_offset = 0.0;
        } else if right_width > right_offset + raw_right {
            right_offset = right_width - raw_right;
        }
        if left_width >= raw_left {
            left_offset = 0.0;
        } else if left_width > left_offset + raw_left {
            left_offset = left_width - raw_left;
        }
    } else {
        left_offset = 0.0;
        right_offset = 0.0;
    }

    PanelFit {
        left_width,
        right_width,
        left_offset,
        right_offset,
    }
}

/// Calculate drag offset for scrollable panel.
pub fn drag_panel_offset(current_offset: f64, raw_width: f64, visible_width: f64, dx: f64) -> f64 {
    if raw_width <= visible_width {
        return current_offset;
    }
    let nonvisible = raw_width - visible_width;
    let new_offset = current_offset + dx;
    new_offset.clamp(-nonvisible, 0.0)
}

/// Calculate visible area of a status bar item.
pub fn item_visible_area(
    is_left: bool,
    panel_width: f64,
    padding: f64,
    offset: f64,
    item_x: f64,
    item_w: f64,
) -> (f64, f64) {
    let mut x = offset + item_x + padding;
    let mut w = item_w;
    if is_left {
        if panel_width - x > 0.0 && panel_width - x < item_w {
            w = (panel_width + padding) - x;
        } else if panel_width - x < 0.0 {
            x = 0.0;
            w = 0.0;
        }
    } else {
        let right_start = panel_width - padding;
        if x < right_start {
            if x + item_w > right_start {
                x = right_start;
                w = (x + item_w) - right_start;
            } else {
                x = 0.0;
                w = 0.0;
            }
        }
    }
    (x, w.max(0.0))
}

// ── Tree model helpers ───────────────────────────────────────────────────────

/// Convert a Lua character class to regex equivalent.
pub fn lua_class_to_regex(ch: char) -> Option<&'static str> {
    match ch {
        'a' => Some("A-Za-z"),
        'A' => Some("^A-Za-z"),
        'd' => Some("0-9"),
        'D' => Some("^0-9"),
        'l' => Some("a-z"),
        'L' => Some("^a-z"),
        'u' => Some("A-Z"),
        'U' => Some("^A-Z"),
        'w' => Some("A-Za-z0-9"),
        'W' => Some("^A-Za-z0-9"),
        's' => Some("\\s"),
        'S' => Some("\\S"),
        'p' => Some(r#"!-/:-@\[-`{-~"#),
        'P' => Some(r#"^!-/:-@\[-`{-~"#),
        'x' => Some("A-Fa-f0-9"),
        'X' => Some("^A-Fa-f0-9"),
        _ => None,
    }
}

/// Escape a character for regex use.
pub fn escape_regex_char(ch: char) -> String {
    match ch {
        '.' | '\\' | '+' | '*' | '?' | '^' | '$' | '(' | ')' | '[' | ']' | '{' | '}' | '|' => {
            format!("\\{ch}")
        }
        _ => ch.to_string(),
    }
}

/// Parse a Lua-style character class [...] and convert to regex.
pub fn parse_class(chars: &[char], start: usize) -> (String, usize) {
    let mut out = String::from("[");
    let mut idx = start + 1;
    if idx < chars.len() && chars[idx] == '^' {
        out.push('^');
        idx += 1;
    }
    while idx < chars.len() {
        let ch = chars[idx];
        if ch == ']' {
            out.push(']');
            return (out, idx + 1);
        }
        if ch == '%' && idx + 1 < chars.len() {
            let cls = chars[idx + 1];
            if let Some(mapped) = lua_class_to_regex(cls) {
                out.push_str(mapped);
            } else {
                out.push_str(&escape_regex_char(cls));
            }
            idx += 2;
            continue;
        }
        if matches!(ch, '\\' | ']' | '[' | '^') {
            out.push('\\');
        }
        out.push(ch);
        idx += 1;
    }
    ("\\[".to_string(), start + 1)
}

/// Convert a Lua pattern to a PCRE2-compatible regex.
pub fn lua_pattern_to_regex(pattern: &str) -> String {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::new();
    let mut idx = 0usize;
    while idx < chars.len() {
        let ch = chars[idx];
        match ch {
            '%' if idx + 1 < chars.len() => {
                let next = chars[idx + 1];
                match next {
                    'b' => out.push_str(r"\b"),
                    'f' => out.push_str(""),
                    other => {
                        if let Some(mapped) = lua_class_to_regex(other) {
                            if mapped.starts_with('^') {
                                out.push('[');
                                out.push_str(mapped);
                                out.push(']');
                            } else if mapped == "\\s" || mapped == "\\S" {
                                out.push_str(mapped);
                            } else {
                                out.push('[');
                                out.push_str(mapped);
                                out.push(']');
                            }
                        } else {
                            out.push_str(&escape_regex_char(other));
                        }
                    }
                }
                idx += 2;
            }
            '[' => {
                let (class, next) = parse_class(&chars, idx);
                out.push_str(&class);
                idx = next;
            }
            '.' => {
                out.push('.');
                idx += 1;
            }
            '*' => {
                out.push('*');
                idx += 1;
            }
            '+' => {
                out.push('+');
                idx += 1;
            }
            '-' => {
                out.push_str("*?");
                idx += 1;
            }
            '?' => {
                out.push('?');
                idx += 1;
            }
            '^' | '$' => {
                out.push(ch);
                idx += 1;
            }
            _ => {
                out.push_str(&escape_regex_char(ch));
                idx += 1;
            }
        }
    }
    out
}

/// Check if an ignore rule pattern uses path matching (contains / not at end).
pub fn ignore_rule_uses_path(pattern: &str) -> bool {
    match pattern.find('/') {
        Some(idx) => idx + 1 < pattern.len() && !pattern.ends_with('/') && !pattern.ends_with("/$"),
        None => false,
    }
}

/// Check if an ignore rule matches directories only.
pub fn ignore_rule_matches_dir(pattern: &str) -> bool {
    pattern.ends_with('/') || pattern.ends_with("/$")
}

/// Convert a glob pattern to a PCRE2-compatible regex.
pub fn glob_to_regex(glob: &str) -> String {
    let chars: Vec<char> = glob.chars().collect();
    let mut out = String::new();
    let mut idx = 0usize;
    while idx < chars.len() {
        if idx + 1 < chars.len() && chars[idx] == '*' && chars[idx + 1] == '*' {
            out.push_str(".*");
            idx += 2;
            continue;
        }
        match chars[idx] {
            '*' => out.push_str("[^/]*"),
            '?' => out.push_str("[^/]"),
            ch => out.push_str(&escape_regex_char(ch)),
        }
        idx += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Node model tests ─────────────────────────────────────────────────────

    #[test]
    fn visible_tabs_basic() {
        assert_eq!(visible_tabs(5, 1, 3), 3);
        assert_eq!(visible_tabs(5, 3, 3), 3);
        assert_eq!(visible_tabs(2, 1, 5), 2);
        assert_eq!(visible_tabs(0, 1, 3), 0);
    }

    #[test]
    fn move_tab_clamped() {
        assert_eq!(move_tab_index(5, 1, -1), 1);
        assert_eq!(move_tab_index(5, 5, 1), 5);
        assert_eq!(move_tab_index(5, 3, 1), 4);
    }

    #[test]
    fn wrapped_tab_wraps() {
        assert_eq!(wrapped_tab_index(5, 1, -1), 5);
        assert_eq!(wrapped_tab_index(5, 5, 1), 1);
        assert_eq!(wrapped_tab_index(5, 3, 1), 4);
    }

    #[test]
    fn split_type_regions() {
        assert_eq!(split_type(100.0, 100.0, 20.0, 10.0, 50.0), "left");
        assert_eq!(split_type(100.0, 100.0, 20.0, 90.0, 50.0), "right");
        assert_eq!(split_type(100.0, 100.0, 20.0, 50.0, 25.0), "up");
        assert_eq!(split_type(100.0, 100.0, 20.0, 50.0, 90.0), "down");
        assert_eq!(split_type(100.0, 100.0, 20.0, 50.0, 60.0), "middle");
    }

    // ── Status model tests ───────────────────────────────────────────────────

    #[test]
    fn fit_panels_no_overflow() {
        let fit = fit_panels(1000.0, 200.0, 200.0, 10.0, 0.0, 0.0);
        assert_eq!(fit.left_offset, 0.0);
        assert_eq!(fit.right_offset, 0.0);
    }

    #[test]
    fn drag_panel_no_overflow() {
        assert_eq!(drag_panel_offset(0.0, 100.0, 200.0, 10.0), 0.0);
    }

    #[test]
    fn drag_panel_clamps() {
        let offset = drag_panel_offset(0.0, 300.0, 200.0, -50.0);
        assert!(offset >= -100.0 && offset <= 0.0);
    }

    // ── Tree model tests ─────────────────────────────────────────────────────

    #[test]
    fn lua_pattern_to_regex_basic() {
        assert_eq!(lua_pattern_to_regex("%d+"), "[0-9]+");
        assert_eq!(lua_pattern_to_regex("%a"), "[A-Za-z]");
    }

    #[test]
    fn escape_regex_char_special() {
        assert_eq!(escape_regex_char('.'), "\\.");
        assert_eq!(escape_regex_char('a'), "a");
    }

    #[test]
    fn ignore_rule_uses_path_check() {
        assert!(ignore_rule_uses_path("src/main.rs"));
        assert!(!ignore_rule_uses_path("*.rs"));
        assert!(!ignore_rule_uses_path("build/"));
    }

    #[test]
    fn ignore_rule_matches_dir_check() {
        assert!(ignore_rule_matches_dir("build/"));
        assert!(!ignore_rule_matches_dir("*.rs"));
    }

    #[test]
    fn glob_to_regex_basic() {
        assert_eq!(glob_to_regex("*.rs"), "[^/]*\\.rs");
        assert_eq!(glob_to_regex("**/*.rs"), ".*/[^/]*\\.rs");
    }
}
