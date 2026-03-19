use mlua::prelude::*;

#[derive(Clone, Copy)]
struct PanelFit {
    left_width: f64,
    right_width: f64,
    left_offset: f64,
    right_offset: f64,
}

fn fit_panels(
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

fn drag_panel_offset(current_offset: f64, raw_width: f64, visible_width: f64, dx: f64) -> f64 {
    if raw_width <= visible_width {
        return current_offset;
    }
    let nonvisible = raw_width - visible_width;
    let new_offset = current_offset + dx;
    new_offset.clamp(-nonvisible, 0.0)
}

fn item_visible_area(
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

pub fn make_module(lua: &Lua) -> LuaResult<LuaTable> {
    let module = lua.create_table()?;
    module.set(
        "fit_panels",
        lua.create_function(
            |lua,
             (total_width, raw_left, raw_right, padding, left_offset, right_offset): (
                f64,
                f64,
                f64,
                f64,
                f64,
                f64,
            )| {
                let fit = fit_panels(
                    total_width,
                    raw_left,
                    raw_right,
                    padding,
                    left_offset,
                    right_offset,
                );
                let out = lua.create_table()?;
                out.set("left_width", fit.left_width)?;
                out.set("right_width", fit.right_width)?;
                out.set("left_offset", fit.left_offset)?;
                out.set("right_offset", fit.right_offset)?;
                Ok(out)
            },
        )?,
    )?;
    module.set(
        "drag_panel_offset",
        lua.create_function(
            |_, (current_offset, raw_width, visible_width, dx): (f64, f64, f64, f64)| {
                Ok(drag_panel_offset(
                    current_offset,
                    raw_width,
                    visible_width,
                    dx,
                ))
            },
        )?,
    )?;
    module.set(
        "item_visible_area",
        lua.create_function(
            |_,
             (is_left, panel_width, padding, offset, item_x, item_w): (
                bool,
                f64,
                f64,
                f64,
                f64,
                f64,
            )| {
                let (x, w) =
                    item_visible_area(is_left, panel_width, padding, offset, item_x, item_w);
                Ok((x, w))
            },
        )?,
    )?;
    Ok(module)
}

#[cfg(test)]
mod tests {
    use super::{drag_panel_offset, fit_panels, item_visible_area};

    #[test]
    fn clamps_panels_when_overflowing() {
        let fit = fit_panels(300.0, 200.0, 200.0, 10.0, 0.0, 0.0);
        assert!(fit.left_width + fit.right_width < 400.0);
    }

    #[test]
    fn drags_panel_offsets_with_clamps() {
        assert_eq!(drag_panel_offset(0.0, 300.0, 200.0, -50.0), -50.0);
        assert_eq!(drag_panel_offset(-90.0, 300.0, 200.0, -50.0), -100.0);
    }

    #[test]
    fn computes_visible_item_bounds() {
        assert_eq!(
            item_visible_area(true, 100.0, 10.0, 0.0, 85.0, 20.0),
            (95.0, 15.0)
        );
        assert_eq!(
            item_visible_area(false, 200.0, 10.0, 0.0, 175.0, 30.0),
            (190.0, 30.0)
        );
    }
}
