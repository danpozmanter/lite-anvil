use mlua::prelude::*;

fn visible_tabs(view_count: usize, tab_offset: usize, max_tabs: usize) -> usize {
    if view_count == 0 {
        return 0;
    }
    view_count
        .saturating_sub(tab_offset.saturating_sub(1))
        .min(max_tabs.max(1))
}

fn move_tab_index(view_count: usize, current_index: usize, direction: i64) -> usize {
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

fn wrapped_tab_index(view_count: usize, current_index: usize, direction: i64) -> usize {
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

fn ensure_visible_tab_offset(
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

fn scroll_tab_offset(
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

fn target_tab_width(
    size_x: f64,
    view_count: usize,
    tab_offset: usize,
    max_tabs: usize,
    tab_width: f64,
) -> f64 {
    let visible = visible_tabs(view_count, tab_offset, max_tabs).max(1) as f64;
    let mut width = size_x.max(1.0);
    if view_count > visible as usize {
        width -= 0.0;
    }
    let min_width = width / (max_tabs.max(1) as f64);
    let max_width = width / visible;
    tab_width.clamp(min_width, max_width)
}

fn tab_hit_index(
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

fn split_type(size_x: f64, size_y: f64, tab_height: f64, mouse_x: f64, mouse_y: f64) -> String {
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

fn drag_overlay_tab_position(
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

pub fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    let module = lua.create_table()?;
    module.set(
        "visible_tabs",
        lua.create_function(
            |_, (view_count, tab_offset, max_tabs): (usize, usize, usize)| {
                Ok(visible_tabs(view_count, tab_offset, max_tabs) as i64)
            },
        )?,
    )?;
    module.set(
        "target_tab_width",
        lua.create_function(
            |_,
             (size_x, view_count, tab_offset, max_tabs, tab_width): (
                f64,
                usize,
                usize,
                usize,
                f64,
            )| {
                Ok(target_tab_width(
                    size_x, view_count, tab_offset, max_tabs, tab_width,
                ))
            },
        )?,
    )?;
    module.set(
        "move_tab_index",
        lua.create_function(
            |_, (view_count, current_index, direction): (usize, usize, i64)| {
                Ok(move_tab_index(view_count, current_index, direction) as i64)
            },
        )?,
    )?;
    module.set(
        "wrapped_tab_index",
        lua.create_function(
            |_, (view_count, current_index, direction): (usize, usize, i64)| {
                Ok(wrapped_tab_index(view_count, current_index, direction) as i64)
            },
        )?,
    )?;
    module.set(
        "ensure_visible_tab_offset",
        lua.create_function(
            |_, (view_count, tab_offset, max_tabs, active_index): (usize, usize, usize, usize)| {
                Ok(
                    ensure_visible_tab_offset(view_count, tab_offset, max_tabs, active_index)
                        as i64,
                )
            },
        )?,
    )?;
    module.set(
        "scroll_tab_offset",
        lua.create_function(
            |_,
             (view_count, tab_offset, max_tabs, active_index, direction): (
                usize,
                usize,
                usize,
                usize,
                i64,
            )| {
                let (offset, active) =
                    scroll_tab_offset(view_count, tab_offset, max_tabs, active_index, direction);
                Ok((offset as i64, active as i64))
            },
        )?,
    )?;
    module.set(
        "tab_hit_index",
        lua.create_function(
            |_,
             (view_count, tab_offset, max_tabs, tab_width, tab_shift, max_width, px): (
                usize,
                usize,
                usize,
                f64,
                f64,
                f64,
                f64,
            )| {
                Ok(tab_hit_index(
                    view_count, tab_offset, max_tabs, tab_width, tab_shift, max_width, px,
                ) as i64)
            },
        )?,
    )?;
    module.set(
        "split_type",
        lua.create_function(
            |_, (size_x, size_y, tab_height, mouse_x, mouse_y): (f64, f64, f64, f64, f64)| {
                Ok(split_type(size_x, size_y, tab_height, mouse_x, mouse_y))
            },
        )?,
    )?;
    module.set(
        "drag_overlay_tab_position",
        lua.create_function(
            |_,
             (
                view_count,
                tab_offset,
                max_tabs,
                tab_width,
                tab_shift,
                max_width,
                px,
                dragged_index,
            ): (usize, usize, usize, f64, f64, f64, f64, usize)| {
                let (index, x, width) = drag_overlay_tab_position(
                    view_count,
                    tab_offset,
                    max_tabs,
                    tab_width,
                    tab_shift,
                    max_width,
                    px,
                    dragged_index,
                );
                Ok((index as i64, x, width))
            },
        )?,
    )?;
    Ok(module)
}

#[cfg(test)]
mod tests {
    use super::{
        drag_overlay_tab_position, ensure_visible_tab_offset, move_tab_index, scroll_tab_offset,
        split_type, tab_hit_index, target_tab_width, visible_tabs, wrapped_tab_index,
    };

    #[test]
    fn computes_visible_tabs() {
        assert_eq!(visible_tabs(10, 3, 8), 8);
        assert_eq!(visible_tabs(2, 1, 8), 2);
    }

    #[test]
    fn computes_target_width() {
        let width = target_tab_width(800.0, 4, 1, 8, 170.0);
        assert!(width > 0.0);
    }

    #[test]
    fn moves_tab_indices_with_clamps() {
        assert_eq!(move_tab_index(4, 2, -1), 1);
        assert_eq!(move_tab_index(4, 2, 1), 3);
        assert_eq!(move_tab_index(4, 1, -1), 1);
        assert_eq!(move_tab_index(4, 4, 1), 4);
    }

    #[test]
    fn wraps_tab_indices() {
        assert_eq!(wrapped_tab_index(4, 1, -1), 4);
        assert_eq!(wrapped_tab_index(4, 4, 1), 1);
        assert_eq!(wrapped_tab_index(4, 2, 1), 3);
    }

    #[test]
    fn keeps_active_tab_visible() {
        assert_eq!(ensure_visible_tab_offset(10, 5, 4, 3), 3);
        assert_eq!(ensure_visible_tab_offset(10, 1, 4, 8), 5);
    }

    #[test]
    fn scrolls_tab_window_and_adjusts_active_tab() {
        assert_eq!(scroll_tab_offset(10, 4, 4, 8, -1), (3, 6));
        assert_eq!(scroll_tab_offset(10, 4, 4, 2, 1), (5, 5));
    }

    #[test]
    fn resolves_split_types() {
        assert_eq!(split_type(100.0, 100.0, 20.0, 10.0, 40.0), "left");
        assert_eq!(split_type(100.0, 100.0, 20.0, 90.0, 40.0), "right");
        assert_eq!(split_type(100.0, 100.0, 20.0, 50.0, 10.0), "tab");
    }

    #[test]
    fn resolves_tab_hits_and_drag_targets() {
        assert_eq!(tab_hit_index(5, 1, 4, 100.0, 0.0, 400.0, 150.0), 2);
        assert_eq!(
            drag_overlay_tab_position(5, 1, 4, 100.0, 0.0, 400.0, 250.0, 0),
            (3, 200.0, 100.0)
        );
    }
}
