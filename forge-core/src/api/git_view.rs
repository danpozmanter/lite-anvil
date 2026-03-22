use mlua::prelude::*;

/// Thin Lua skeleton for `core.git.ui` — extends View, sets context, calls native populate.
const UI_BOOTSTRAP: &str = r#"local View = require "core.view"
local native = require "native_git_view"
local DiffView   = View:extend()
local StatusView = View:extend()
function DiffView:__tostring()   return "GitDiffView"   end
function StatusView:__tostring() return "GitStatusView" end
DiffView.context   = "session"
StatusView.context = "session"
native.populate_diff(DiffView)
native.populate_status(StatusView)
return native.make_ui(DiffView, StatusView)"#;

fn require_table(lua: &Lua, name: &str) -> LuaResult<LuaTable> {
    let require: LuaFunction = lua.globals().get("require")?;
    require.call(name)
}

/// Call a Lua class constructor (`Class(...)`) via a Lua trampoline.
fn call_class(lua: &Lua, class: &LuaTable, args: impl IntoLuaMulti) -> LuaResult<LuaValue> {
    let trampoline: LuaFunction = lua
        .load("return function(cls, ...) return cls(...) end")
        .eval()?;
    let mut call_args = vec![LuaValue::Table(class.clone())];
    for v in args.into_lua_multi(lua)?.into_iter() {
        call_args.push(v);
    }
    trampoline.call::<LuaValue>(LuaMultiValue::from_vec(call_args))
}

fn populate_diff(lua: &Lua, class: LuaTable) -> LuaResult<()> {
    use std::sync::Arc;

    let class_key = Arc::new(lua.create_registry_value(class.clone())?);

    // new(self, title, text)
    class.set("new", {
        let k = Arc::clone(&class_key);
        lua.create_function(move |lua, (this, title, text): (LuaTable, String, String)| {
            let class: LuaTable = lua.registry_value(&k)?;
            let super_tbl: LuaTable = class.get("super")?;
            let super_new: LuaFunction = super_tbl.get("new")?;
            super_new.call::<()>(this.clone())?;

            this.set("scrollable", true)?;
            this.set("title", title)?;

            let lines_tbl = lua.create_table()?;
            let input = text + "\n";
            for line in input.split('\n') {
                // split('\n') on "a\nb\n" gives ["a", "b", ""], skip trailing empty
                lines_tbl.push(line.to_owned())?;
            }
            // Remove the trailing empty string that split produces
            let len = lines_tbl.raw_len();
            if len > 0 {
                let last: String = lines_tbl.get(len as i64).unwrap_or_default();
                if last.is_empty() {
                    lines_tbl.raw_set(len as i64, LuaValue::Nil)?;
                }
            }
            this.set("lines", lines_tbl)?;
            Ok(())
        })?
    })?;

    // get_name(self)
    class.set("get_name", lua.create_function(|_lua, this: LuaTable| {
        this.get::<String>("title")
    })?)?;

    // get_line_height(self)
    class.set("get_line_height", lua.create_function(|lua, _this: LuaTable| {
        let style: LuaTable = require_table(lua, "core.style")?;
        let code_font: LuaValue = style.get("code_font")?;
        let h: f64 = match &code_font {
            LuaValue::Table(t) => t.call_method("get_height", ())?,
            LuaValue::UserData(ud) => ud.call_method("get_height", ())?,
            _ => return Err(LuaError::RuntimeError("code_font invalid".into())),
        };
        let padding: LuaTable = style.get("padding")?;
        let py: f64 = padding.get("y")?;
        Ok(h + py)
    })?)?;

    // get_scrollable_size(self)
    class.set("get_scrollable_size", lua.create_function(|lua, this: LuaTable| {
        let lines: LuaTable = this.get("lines")?;
        let n = lines.raw_len() as f64;
        let lh: f64 = this.call_method("get_line_height", ())?;
        let style: LuaTable = require_table(lua, "core.style")?;
        let padding: LuaTable = style.get("padding")?;
        let py: f64 = padding.get("y")?;
        Ok(n * lh + py * 2.0)
    })?)?;

    // draw(self)
    class.set("draw", lua.create_function(|lua, this: LuaTable| {
        let style: LuaTable = require_table(lua, "core.style")?;
        let bg: LuaValue = style.get("background")?;
        this.call_method::<()>("draw_background", bg)?;

        let (ox, oy): (f64, f64) = this.call_method("get_content_offset", ())?;
        let lh: f64 = this.call_method("get_line_height", ())?;

        let scroll: LuaTable = this.get("scroll")?;
        let scroll_y: f64 = scroll.get("y")?;
        let size: LuaTable = this.get("size")?;
        let size_y: f64 = size.get("y")?;

        let min = ((scroll_y / lh).floor() as i64).max(1);
        let max = min + (size_y / lh).floor() as i64 + 1;

        let padding: LuaTable = style.get("padding")?;
        let px: f64 = padding.get("x")?;
        let py: f64 = padding.get("y")?;
        let mut y = oy + py + lh * (min - 1) as f64;

        let lines: LuaTable = this.get("lines")?;
        let common: LuaTable = require_table(lua, "core.common")?;
        let code_font: LuaValue = style.get("code_font")?;
        let color_text: LuaValue = style.get("text")?;
        let color_accent: LuaValue = style.get("accent")?;
        let color_good: LuaValue = style.get("good").unwrap_or_else(|_| {
            // fallback: { 120, 220, 120, 255 }
            LuaValue::Nil // will use text color
        });
        let color_good = if matches!(color_good, LuaValue::Nil) {
            color_text.clone()
        } else {
            color_good
        };
        let color_error: LuaValue = style.get("error").unwrap_or(LuaValue::Nil);
        let color_error = if matches!(color_error, LuaValue::Nil) {
            color_text.clone()
        } else {
            color_error
        };
        let color_dim: LuaValue = style.get("dim")?;
        let size_x: f64 = size.get("x")?;

        for i in min..=max {
            let line: Option<String> = lines.get(i)?;
            let line = match line { Some(l) => l, None => break };

            let color = if line.starts_with("@@") {
                color_accent.clone()
            } else if line.starts_with('+') && !line.starts_with("++") {
                color_good.clone()
            } else if line.starts_with('-') && !line.starts_with("--") {
                color_error.clone()
            } else if line.starts_with("diff ")
                || line.starts_with("index ")
                || line.starts_with("--- ")
                || line.starts_with("+++ ")
            {
                color_dim.clone()
            } else {
                color_text.clone()
            };

            common.call_function::<()>("draw_text", (
                code_font.clone(), color, line, "left",
                ox + px, y, size_x, lh,
            ))?;
            y += lh;
        }

        this.call_method::<()>("draw_scrollbar", ())
    })?)?;

    Ok(())
}

fn populate_status(lua: &Lua, class: LuaTable) -> LuaResult<()> {
    use std::sync::Arc;

    let class_key = Arc::new(lua.create_registry_value(class.clone())?);

    // new(self, root)
    class.set("new", {
        let k = Arc::clone(&class_key);
        lua.create_function(move |lua, (this, root): (LuaTable, String)| {
            let class: LuaTable = lua.registry_value(&k)?;
            let super_tbl: LuaTable = class.get("super")?;
            let super_new: LuaFunction = super_tbl.get("new")?;
            super_new.call::<()>(this.clone())?;

            this.set("scrollable", true)?;
            this.set("repo_root", root.clone())?;
            this.set("selected_idx", 1i64)?;

            let git: LuaTable = require_table(lua, "core.git")?;
            git.call_function::<()>("refresh", (root, true))?;
            Ok(())
        })?
    })?;

    // get_repo(self)
    class.set("get_repo", lua.create_function(|lua, this: LuaTable| {
        let git: LuaTable = require_table(lua, "core.git")?;
        let root: String = this.get("repo_root")?;
        let r: LuaValue = git.call_function("refresh", (root.clone(), false))?;
        if !matches!(r, LuaValue::Nil | LuaValue::Boolean(false)) {
            return Ok(r);
        }
        git.call_function("get_repo", root)
    })?)?;

    // get_items(self)
    class.set("get_items", lua.create_function(|lua, this: LuaTable| {
        let repo: LuaValue = this.call_method("get_repo", ())?;
        match repo {
            LuaValue::Table(r) => {
                let ordered: LuaValue = r.get("ordered")?;
                if matches!(ordered, LuaValue::Nil) {
                    Ok(LuaValue::Table(lua.create_table()?))
                } else {
                    Ok(ordered)
                }
            }
            _ => Ok(LuaValue::Table(lua.create_table()?)),
        }
    })?)?;

    // get_name(self)
    class.set("get_name", lua.create_function(|lua, this: LuaTable| {
        let repo: LuaValue = this.call_method("get_repo", ())?;
        let branch = if let LuaValue::Table(r) = &repo {
            r.get::<Option<String>>("branch")?.unwrap_or_default()
        } else {
            String::new()
        };
        let _ = lua;
        Ok(format!("Git Status [{}]", if branch.is_empty() { "git".to_string() } else { branch }))
    })?)?;

    // get_line_height(self)
    class.set("get_line_height", lua.create_function(|lua, _this: LuaTable| {
        let style: LuaTable = require_table(lua, "core.style")?;
        let font: LuaValue = style.get("font")?;
        let h: f64 = match &font {
            LuaValue::Table(t) => t.call_method("get_height", ())?,
            LuaValue::UserData(ud) => ud.call_method("get_height", ())?,
            _ => return Err(LuaError::RuntimeError("style.font invalid".into())),
        };
        let padding: LuaTable = style.get("padding")?;
        let py: f64 = padding.get("y")?;
        Ok(h + py + 2.0)
    })?)?;

    // get_header_height(self)
    class.set("get_header_height", lua.create_function(|lua, _this: LuaTable| {
        let style: LuaTable = require_table(lua, "core.style")?;
        let font: LuaValue = style.get("font")?;
        let h: f64 = match &font {
            LuaValue::Table(t) => t.call_method("get_height", ())?,
            LuaValue::UserData(ud) => ud.call_method("get_height", ())?,
            _ => return Err(LuaError::RuntimeError("style.font invalid".into())),
        };
        let padding: LuaTable = style.get("padding")?;
        let py: f64 = padding.get("y")?;
        Ok(h * 2.0 + py * 3.0)
    })?)?;

    // get_scrollable_size(self)
    class.set("get_scrollable_size", lua.create_function(|lua, this: LuaTable| {
        let items: LuaTable = match this.call_method("get_items", ())? {
            LuaValue::Table(t) => t,
            _ => return Err(LuaError::RuntimeError("get_items returned non-table".into())),
        };
        let n = items.raw_len() as f64;
        let header_h: f64 = this.call_method("get_header_height", ())?;
        let lh: f64 = this.call_method("get_line_height", ())?;
        let _ = lua;
        Ok(header_h + n * lh)
    })?)?;

    // each_visible_item(self) — uses coroutine.wrap in Lua; we return an iterator fn here
    class.set("each_visible_item", lua.create_function(|lua, this: LuaTable| {
        let items: LuaTable = match this.call_method("get_items", ())? {
            LuaValue::Table(t) => t,
            _ => return Err(LuaError::RuntimeError("get_items returned non-table".into())),
        };
        let lh: f64 = this.call_method("get_line_height", ())?;
        let (x, y_base): (f64, f64) = this.call_method("get_content_offset", ())?;

        let scroll: LuaTable = this.get("scroll")?;
        let scroll_y: f64 = scroll.get("y")?;
        let size: LuaTable = this.get("size")?;
        let size_y: f64 = size.get("y")?;

        let style: LuaTable = require_table(lua, "core.style")?;
        let font: LuaValue = style.get("font")?;
        let font_h: f64 = match &font {
            LuaValue::Table(t) => t.call_method("get_height", ())?,
            LuaValue::UserData(ud) => ud.call_method("get_height", ())?,
            _ => return Err(LuaError::RuntimeError("style.font invalid".into())),
        };

        let header_h: f64 = this.call_method("get_header_height", ())?;

        let min = (((scroll_y - font_h) / lh).floor() as i64).max(1);
        let max = min + (size_y / lh).floor() as i64 + 1;
        let mut y = y_base + header_h + lh * (min - 1) as f64;

        // We need get_content_bounds for width
        let bounds: LuaMultiValue = this.call_method("get_content_bounds", ())?;
        let mut b_iter = bounds.into_iter();
        let _bx: f64 = match b_iter.next() { Some(LuaValue::Number(n)) => n, _ => 0.0 };
        let _by: f64 = match b_iter.next() { Some(LuaValue::Number(n)) => n, _ => 0.0 };
        let w: f64 = match b_iter.next() { Some(LuaValue::Number(n)) => n, _ => size.get("x")? };

        // Materialize visible items into a sequence for the iterator to return
        let result = lua.create_table()?;
        let mut out_idx = 0i64;
        for i in min..=max {
            let item: LuaValue = items.get(i)?;
            if matches!(item, LuaValue::Nil) { break; }
            out_idx += 1;
            let entry = lua.create_table()?;
            entry.set(1, i)?;
            entry.set(2, item)?;
            entry.set(3, x)?;
            entry.set(4, y)?;
            entry.set(5, w)?;
            entry.set(6, lh)?;
            result.set(out_idx, entry)?;
            y += lh;
        }

        // Return an iterator closure over the pre-computed result table
        use std::sync::Arc;
        use parking_lot::Mutex;
        let result_key = Arc::new(lua.create_registry_value(result)?);
        let idx = Arc::new(Mutex::new(0i64));
        let iter_fn = lua.create_function(move |lua, ()| {
            let mut i = idx.lock();
            *i += 1;
            let result: LuaTable = lua.registry_value(&result_key)?;
            let entry: LuaValue = result.get(*i)?;
            match entry {
                LuaValue::Table(e) => {
                    let a: LuaValue = e.get(1)?;
                    let b: LuaValue = e.get(2)?;
                    let c: f64 = e.get(3)?;
                    let d: f64 = e.get(4)?;
                    let f: f64 = e.get(5)?;
                    let g: f64 = e.get(6)?;
                    Ok(LuaMultiValue::from_vec(vec![a, b, LuaValue::Number(c), LuaValue::Number(d), LuaValue::Number(f), LuaValue::Number(g)]))
                }
                _ => Ok(LuaMultiValue::new()),
            }
        })?;
        Ok(LuaValue::Function(iter_fn))
    })?)?;

    // scroll_to_selected(self)
    class.set("scroll_to_selected", lua.create_function(|lua, this: LuaTable| {
        let selected_idx: i64 = this.get("selected_idx")?;
        let lh: f64 = this.call_method("get_line_height", ())?;
        let header_h: f64 = this.call_method("get_header_height", ())?;
        let y = (selected_idx - 1) as f64 * lh;
        let size: LuaTable = this.get("size")?;
        let size_y: f64 = size.get("y")?;
        let scroll: LuaTable = this.get("scroll")?;
        let scroll_to: LuaTable = scroll.get("to")?;
        let cur: f64 = scroll_to.get("y")?;
        let new_y = cur.min(y).max(y + lh - size_y + header_h);
        scroll_to.set("y", new_y)?;
        let _ = lua;
        Ok(())
    })?)?;

    // get_selected(self)
    class.set("get_selected", lua.create_function(|lua, this: LuaTable| {
        let items: LuaTable = match this.call_method("get_items", ())? {
            LuaValue::Table(t) => t,
            _ => return Err(LuaError::RuntimeError("get_items returned non-table".into())),
        };
        let idx: i64 = this.get("selected_idx")?;
        let _ = lua;
        items.get::<LuaValue>(idx)
    })?)?;

    // open_selected(self)
    class.set("open_selected", lua.create_function(|lua, this: LuaTable| {
        let item: LuaValue = this.call_method("get_selected", ())?;
        let item = match item {
            LuaValue::Table(t) => t,
            _ => return Ok(()),
        };
        let path: String = item.get("path")?;
        let core: LuaTable = require_table(lua, "core")?;
        let open_doc: LuaFunction = core.get("open_doc")?;
        let doc = open_doc.call::<LuaValue>(path)?;
        let root_view: LuaTable = core.get("root_view")?;
        root_view.call_method::<()>("open_doc", doc)
    })?)?;

    // draw(self) — header + item list
    class.set("draw", lua.create_function(|lua, this: LuaTable| {
        let style: LuaTable = require_table(lua, "core.style")?;
        let bg: LuaValue = style.get("background")?;
        this.call_method::<()>("draw_background", bg)?;

        let repo: LuaValue = this.call_method("get_repo", ())?;
        let mut header = "No repository".to_string();
        let mut detail = String::new();

        if let LuaValue::Table(r) = &repo {
            let error: Option<String> = r.get("error")?;
            if let Some(e) = error.filter(|e| !e.is_empty()) {
                header = format!("Git error: {}", e);
            } else {
                let ahead: i64 = r.get("ahead").unwrap_or(0);
                let behind: i64 = r.get("behind").unwrap_or(0);
                let branch: String = r.get("branch").unwrap_or_default();
                let refreshing: bool = r.get("refreshing").unwrap_or(false);
                let mut summary = vec![];
                if ahead > 0 { summary.push(format!("ahead {}", ahead)); }
                if behind > 0 { summary.push(format!("behind {}", behind)); }
                header = if !branch.is_empty() {
                    branch.clone()
                } else if refreshing {
                    "Refreshing Git status...".to_string()
                } else {
                    "(no branch)".to_string()
                };
                detail = summary.join("  ");
                if refreshing && !branch.is_empty() {
                    detail = if detail.is_empty() {
                        "refreshing...".to_string()
                    } else {
                        format!("{}  refreshing...", detail)
                    };
                }
            }
        }

        let position: LuaTable = this.get("position")?;
        let ox: f64 = position.get("x")?;
        let oy: f64 = position.get("y")?;
        let size: LuaTable = this.get("size")?;
        let size_x: f64 = size.get("x")?;
        let header_h: f64 = this.call_method("get_header_height", ())?;
        let section_inset = 10.0f64;

        let renderer: LuaTable = lua.globals().get("renderer")?;
        let font: LuaValue = style.get("font")?;
        let font_h: f64 = match &font {
            LuaValue::Table(t) => t.call_method("get_height", ())?,
            LuaValue::UserData(ud) => ud.call_method("get_height", ())?,
            _ => return Err(LuaError::RuntimeError("style.font invalid".into())),
        };
        let padding: LuaTable = style.get("padding")?;
        let py: f64 = padding.get("y")?;
        let color_text: LuaValue = style.get("text")?;
        let color_dim: LuaValue = style.get("dim")?;

        renderer.call_function::<()>("draw_rect", (ox, oy, size_x, header_h, style.get::<LuaValue>("background")?))?;
        renderer.call_function::<()>("draw_text", (font.clone(), header.clone(), ox + section_inset, oy + py, color_text.clone()))?;
        if !detail.is_empty() {
            renderer.call_function::<()>("draw_text", (
                font.clone(), detail.clone(),
                ox + section_inset, oy + py + font_h + 2.0, color_dim.clone(),
            ))?;
        }

        let divider_size: f64 = style.get("divider_size").unwrap_or(1.0);
        let divider_color: LuaValue = style.get("divider")?;
        renderer.call_function::<()>("draw_rect", (
            ox + section_inset, oy + header_h - py,
            size_x - section_inset * 2.0, divider_size, divider_color,
        ))?;

        let items: LuaTable = match this.call_method("get_items", ())? {
            LuaValue::Table(t) => t,
            _ => return Err(LuaError::RuntimeError("get_items non-table".into())),
        };

        if let LuaValue::Table(r) = &repo {
            let error: Option<String> = r.get("error")?;
            if let Some(e) = error.filter(|e| !e.is_empty()) {
                let color_error: LuaValue = style.get("error").unwrap_or_else(|_| color_text.clone());
                renderer.call_function::<()>("draw_text", (
                    font.clone(), e, ox + section_inset, oy + header_h + py, color_error,
                ))?;
            } else if items.raw_len() == 0 {
                let refreshing: bool = r.get("refreshing").unwrap_or(false);
                if !refreshing {
                    renderer.call_function::<()>("draw_text", (
                        font.clone(), "Working tree clean",
                        ox + section_inset, oy + header_h + py, color_dim.clone(),
                    ))?;
                }
            }
        }

        let selected_idx: i64 = this.get("selected_idx")?;
        let iter_fn: LuaFunction = this.call_method("each_visible_item", ())?;
        loop {
            let results: LuaMultiValue = iter_fn.call(())?;
            let mut vals = results.into_iter();
            let i_val = match vals.next() { Some(v) if !matches!(v, LuaValue::Nil) => v, _ => break };
            let i: i64 = match i_val { LuaValue::Integer(n) => n, _ => break };
            let item: LuaTable = match vals.next() {
                Some(LuaValue::Table(t)) => t,
                _ => break,
            };
            let x: f64 = match vals.next() { Some(LuaValue::Number(n)) => n, _ => break };
            let y: f64 = match vals.next() { Some(LuaValue::Number(n)) => n, _ => break };
            let w: f64 = match vals.next() { Some(LuaValue::Number(n)) => n, _ => break };
            let h: f64 = match vals.next() { Some(LuaValue::Number(n)) => n, _ => break };

            if i == selected_idx {
                let hi_base: LuaValue = style.get("line_highlight")?;
                if let LuaValue::Table(hi_t) = hi_base {
                    // Copy table and boost alpha
                    let hi = lua.create_table()?;
                    for j in 1..=4i64 {
                        let v: LuaValue = hi_t.get(j).unwrap_or(LuaValue::Integer(0));
                        hi.set(j, v)?;
                    }
                    let alpha: i64 = hi.get(4).unwrap_or(0);
                    hi.set(4i64, alpha.max(190))?;
                    renderer.call_function::<()>("draw_rect", (x, y, w, h, LuaValue::Table(hi)))?;
                }
            } else if i % 2 == 0 {
                let stripe_base: LuaValue = style.get("background2")?;
                if let LuaValue::Table(st) = stripe_base {
                    let stripe = lua.create_table()?;
                    for j in 1..=4i64 {
                        let v: LuaValue = st.get(j).unwrap_or(LuaValue::Integer(0));
                        stripe.set(j, v)?;
                    }
                    stripe.set(4i64, 70i64)?;
                    renderer.call_function::<()>("draw_rect", (x, y, w, h, LuaValue::Table(stripe)))?;
                }
            }

            let kind: String = item.get("kind").unwrap_or_default();
            let code_color = match kind.as_str() {
                "staged"    => style.get("accent")?,
                "changed"   => color_text.clone(),
                "untracked" => style.get("good").unwrap_or_else(|_| LuaValue::Nil),
                "conflict"  => style.get("error").unwrap_or_else(|_| LuaValue::Nil),
                _           => color_dim.clone(),
            };
            let code_color = if matches!(code_color, LuaValue::Nil) { color_dim.clone() } else { code_color };

            let rel: String = {
                let common: LuaTable = require_table(lua, "core.common")?;
                let raw_rel: String = item.get("rel")?;
                common.call_function("home_encode", raw_rel)?
            };
            let code: String = item.get("code").unwrap_or_default();
            let dx = x + section_inset;
            let code_font: LuaValue = style.get("code_font")?;
            let font_h_col = font_h * 1.4;
            let common: LuaTable = require_table(lua, "core.common")?;
            common.call_function::<()>("draw_text", (
                code_font.clone(), code_color, code, "center",
                dx, y, font_h_col, h,
            ))?;
            common.call_function::<()>("draw_text", (
                font.clone(), color_text.clone(), rel, "left",
                dx + font_h * 1.7, y, (w - dx - section_inset).max(0.0), h,
            ))?;
        }

        this.call_method::<()>("draw_scrollbar", ())
    })?)?;

    Ok(())
}

fn make_ui(lua: &Lua, diff_class: LuaTable, status_class: LuaTable) -> LuaResult<LuaTable> {
    use std::sync::Arc;

    let diff_key = Arc::new(lua.create_registry_value(diff_class.clone())?);
    let status_key = Arc::new(lua.create_registry_value(status_class.clone())?);

    let ui = lua.create_table()?;

    ui.set("DiffView", diff_class)?;
    ui.set("StatusView", status_class)?;

    // open_status(root)
    ui.set("open_status", {
        let sk = Arc::clone(&status_key);
        lua.create_function(move |lua, root: LuaValue| {
            let root_str: Option<String> = match &root {
                LuaValue::String(s) => Some(s.to_str()?.to_owned()),
                LuaValue::Nil => None,
                _ => None,
            };
            let root_str = match root_str {
                Some(r) => r,
                None => {
                    let git: LuaTable = require_table(lua, "core.git")?;
                    let repo: LuaValue = git.call_function("get_active_repo", ())?;
                    match repo {
                        LuaValue::Table(r) => {
                            let root: Option<String> = r.get("root")?;
                            match root {
                                Some(r) => r,
                                None => {
                                    let core: LuaTable = require_table(lua, "core")?;
                                    core.call_function::<()>("error", "Not inside a Git repository")?;
                                    return Ok(LuaValue::Nil);
                                }
                            }
                        }
                        _ => {
                            let core: LuaTable = require_table(lua, "core")?;
                            core.call_function::<()>("error", "Not inside a Git repository")?;
                            return Ok(LuaValue::Nil);
                        }
                    }
                }
            };
            let status_class: LuaTable = lua.registry_value(&sk)?;
            let view: LuaValue = call_class(lua, &status_class, root_str)?;
            let core: LuaTable = require_table(lua, "core")?;
            let root_view: LuaTable = core.get("root_view")?;
            let node: LuaTable = root_view.call_method("get_active_node_default", ())?;
            node.call_method::<()>("add_view", view.clone())?;
            Ok(view)
        })?
    })?;

    // open_repo_diff(root, cached)
    ui.set("open_repo_diff", {
        let dk = Arc::clone(&diff_key);
        lua.create_function(move |lua, (root, cached): (LuaValue, bool)| {
            let root_str: String = match &root {
                LuaValue::String(s) => s.to_str()?.to_owned(),
                _ => {
                    let git: LuaTable = require_table(lua, "core.git")?;
                    let repo: LuaValue = git.call_function("get_active_repo", ())?;
                    match repo {
                        LuaValue::Table(r) => r.get("root").unwrap_or_default(),
                        _ => {
                            let core: LuaTable = require_table(lua, "core")?;
                            core.call_function::<()>("error", "Not inside a Git repository")?;
                            return Ok(());
                        }
                    }
                }
            };
            let diff_key = Arc::clone(&dk);
            let git: LuaTable = require_table(lua, "core.git")?;
            let title = format!("Git Diff{}", if cached { " [staged]" } else { "" });
            let callback = lua.create_function(move |lua, (ok, stdout, stderr): (bool, String, String)| {
                if !ok {
                    let core: LuaTable = require_table(lua, "core")?;
                    let msg = if !stderr.is_empty() { stderr } else { "git diff failed".to_string() };
                    return core.call_function::<()>("error", msg);
                }
                let text = if !stdout.is_empty() { stdout } else { "No diff".to_string() };
                let diff_class: LuaTable = lua.registry_value(&diff_key)?;
                let view: LuaValue = call_class(lua, &diff_class, (title.clone(), text))?;
                let core: LuaTable = require_table(lua, "core")?;
                let root_view: LuaTable = core.get("root_view")?;
                let node: LuaTable = root_view.call_method("get_active_node_default", ())?;
                node.call_method::<()>("add_view", view)
            })?;
            git.call_function::<()>("diff_repo", (root_str, cached, callback))
        })?
    })?;

    // open_file_diff(path, cached)
    ui.set("open_file_diff", {
        let dk = Arc::clone(&diff_key);
        lua.create_function(move |lua, (path, cached): (String, bool)| {
            let diff_key = Arc::clone(&dk);
            let git: LuaTable = require_table(lua, "core.git")?;
            let common: LuaTable = require_table(lua, "core.common")?;
            let basename: String = common.call_function("basename", path.clone())?;
            let title = format!("{}.diff", basename);
            let callback = lua.create_function(move |lua, (ok, stdout, stderr): (bool, String, String)| {
                if !ok {
                    let core: LuaTable = require_table(lua, "core")?;
                    let msg = if !stderr.is_empty() { stderr } else { "git diff failed".to_string() };
                    return core.call_function::<()>("error", msg);
                }
                let text = if !stdout.is_empty() { stdout } else { "No diff".to_string() };
                let diff_class: LuaTable = lua.registry_value(&diff_key)?;
                let view: LuaValue = call_class(lua, &diff_class, (title.clone(), text))?;
                let core: LuaTable = require_table(lua, "core")?;
                let root_view: LuaTable = core.get("root_view")?;
                let node: LuaTable = root_view.call_method("get_active_node_default", ())?;
                node.call_method::<()>("add_view", view)
            })?;
            git.call_function::<()>("diff_file", (path, cached, callback))
        })?
    })?;

    Ok(ui)
}

/// Registers `core.git.ui` (thin skeleton) and `native_git_view` (Rust populate + make_ui).
pub fn register_preload(lua: &Lua) -> LuaResult<()> {
    let preload: LuaTable = lua.globals().get::<LuaTable>("package")?.get("preload")?;

    preload.set(
        "core.git.ui",
        lua.create_function(|lua, ()| {
            lua.load(UI_BOOTSTRAP).set_name("core.git.ui").eval::<LuaValue>()
        })?,
    )?;

    preload.set(
        "native_git_view",
        lua.create_function(|lua, ()| {
            let t = lua.create_table()?;
            t.set("populate_diff", lua.create_function(|lua, class: LuaTable| {
                populate_diff(lua, class)
            })?)?;
            t.set("populate_status", lua.create_function(|lua, class: LuaTable| {
                populate_status(lua, class)
            })?)?;
            t.set("make_ui", lua.create_function(|lua, (diff, status): (LuaTable, LuaTable)| {
                make_ui(lua, diff, status)
            })?)?;
            Ok(LuaValue::Table(t))
        })?,
    )
}
