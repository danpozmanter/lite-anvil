mod cache;
mod font;

pub use font::RenFont;

use cache::{RenCache, RenColor, RenRect};
use font::{FontInner, FontRef, parse_font_opts};
use mlua::prelude::*;
use sdl3_sys::everything::*;

// ── Thread-local renderer state ───────────────────────────────────────────────

thread_local! {
    static CACHE: std::cell::RefCell<Option<RenCache>> =
        const { std::cell::RefCell::new(None) };
}

fn with_cache<F: FnOnce(&mut RenCache)>(f: F) {
    CACHE.with(|c| {
        let mut borrow = c.borrow_mut();
        if borrow.is_none() {
            *borrow = Some(RenCache::new());
        }
        f(borrow.as_mut().unwrap());
    });
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Build the `renderer` Lua table with a real SDL2 + FreeType2 backend.
pub fn make_renderer(lua: &Lua) -> LuaResult<LuaTable> {
    let r = lua.create_table()?;

    // ── renderer.font ──────────────────────────────────────────────────────
    //
    // Font instances are Lua tables {[1] = RenFont_userdata} with renderer.font
    // as their metatable. This makes Object.is(font, renderer.font) work correctly
    // since Object.is checks getmetatable(obj) == renderer.font.

    let font_tbl = lua.create_table()?;

    // Self-referential __index so method calls on font instances resolve here.
    font_tbl.set("__index", font_tbl.clone())?;

    // Registry keys so closures that create new font tables can access the metatable.
    let fmk_load = lua.create_registry_value(font_tbl.clone())?;
    let fmk_copy = lua.create_registry_value(font_tbl.clone())?;
    let fmk_group = lua.create_registry_value(font_tbl.clone())?;

    // load(path, size [, opts]) → font table
    font_tbl.set(
        "load",
        lua.create_function(
            move |lua, (path, size, opts): (String, f32, Option<LuaTable>)| {
                let (aa, h) = opts
                    .as_ref()
                    .map(parse_font_opts)
                    .transpose()
                    .map_err(LuaError::external)?
                    .unwrap_or((None, None));
                let inner =
                    FontInner::load(&path, size, aa.unwrap_or_default(), h.unwrap_or_default())
                        .map_err(LuaError::external)?;
                let meta: LuaTable = lua.registry_value(&fmk_load)?;
                wrap_font(lua, RenFont::new(inner), meta)
            },
        )?,
    )?;

    // copy(self [, size [, opts]]) → font table  (also used as instance method)
    font_tbl.set(
        "copy",
        lua.create_function(
            move |lua, (tbl, size, opts): (LuaTable, Option<f32>, Option<LuaTable>)| {
                let ud = ud_from_font_tbl(&tbl)?;
                let rf = ud.borrow::<RenFont>()?;
                let (aa, h) = opts
                    .as_ref()
                    .map(parse_font_opts)
                    .transpose()
                    .map_err(LuaError::external)?
                    .unwrap_or((None, None));
                let new_rf = rf.copy_with(size, aa, h).map_err(LuaError::external)?;
                drop(rf);
                let meta: LuaTable = lua.registry_value(&fmk_copy)?;
                wrap_font(lua, new_rf, meta)
            },
        )?,
    )?;

    // group({f1, f2, ...}) → merged font table
    font_tbl.set(
        "group",
        lua.create_function(move |lua, tbl: LuaTable| {
            let mut refs: Vec<FontRef> = Vec::new();
            for i in 1..=tbl.raw_len() {
                let font_t: LuaTable = tbl.raw_get(i)?;
                let ud = ud_from_font_tbl(&font_t)?;
                let rf = ud.borrow::<RenFont>()?;
                refs.extend_from_slice(&rf.0);
            }
            if refs.is_empty() {
                return Err(LuaError::runtime("renderer.font.group: empty table"));
            }
            let meta: LuaTable = lua.registry_value(&fmk_group)?;
            wrap_font(lua, RenFont(refs), meta)
        })?,
    )?;

    // Instance methods — self is the font table, inner userdata at [1].
    font_tbl.set(
        "get_width",
        lua.create_function(
            |_, (tbl, text, opts): (LuaTable, String, Option<LuaTable>)| {
                let tab_offset = opts
                    .as_ref()
                    .and_then(|t| t.get::<Option<f32>>("tab_offset").ok().flatten())
                    .unwrap_or(0.0);
                let ud = ud_from_font_tbl(&tbl)?;
                let rf = ud.borrow::<RenFont>()?;
                Ok(rf.get_width(&text, tab_offset))
            },
        )?,
    )?;
    font_tbl.set(
        "get_height",
        lua.create_function(|_, tbl: LuaTable| {
            let ud = ud_from_font_tbl(&tbl)?;
            Ok(ud.borrow::<RenFont>()?.get_height())
        })?,
    )?;
    font_tbl.set(
        "get_size",
        lua.create_function(|_, tbl: LuaTable| {
            Ok(ud_from_font_tbl(&tbl)?.borrow::<RenFont>()?.get_size())
        })?,
    )?;
    font_tbl.set(
        "get_tab_size",
        lua.create_function(|_, tbl: LuaTable| {
            Ok(ud_from_font_tbl(&tbl)?.borrow::<RenFont>()?.get_tab_size())
        })?,
    )?;
    font_tbl.set(
        "get_path",
        lua.create_function(|_, tbl: LuaTable| {
            Ok(ud_from_font_tbl(&tbl)?.borrow::<RenFont>()?.get_path())
        })?,
    )?;
    font_tbl.set(
        "set_size",
        lua.create_function(|_, (tbl, size): (LuaTable, f32)| {
            ud_from_font_tbl(&tbl)?.borrow::<RenFont>()?.set_size(size);
            Ok(())
        })?,
    )?;
    font_tbl.set(
        "set_tab_size",
        lua.create_function(|_, (tbl, n): (LuaTable, i32)| {
            ud_from_font_tbl(&tbl)?.borrow::<RenFont>()?.set_tab_size(n);
            Ok(())
        })?,
    )?;

    r.set("font", font_tbl)?;

    // ── renderer.begin_frame ───────────────────────────────────────────────

    r.set(
        "begin_frame",
        lua.create_function(|_, _win: LuaValue| {
            let (w, h) = crate::window::get_drawable_size();
            with_cache(|c| {
                if crate::window::take_needs_invalidate() {
                    c.invalidate();
                }
                c.begin_frame(w, h);
            });
            Ok(())
        })?,
    )?;

    // ── renderer.end_frame ─────────────────────────────────────────────────

    r.set(
        "end_frame",
        lua.create_function(|_, ()| {
            CACHE.with(|cell| {
                let mut borrow = cell.borrow_mut();
                let Some(cache) = borrow.as_mut() else { return };
                let dirty = cache.compute_dirty_rects();
                if dirty.is_empty() {
                    return;
                }
                let commands = &cache.commands;
                crate::window::with_window_surface(|surface, window| {
                    // SAFETY: surface is valid for this call; we're on the main thread.
                    unsafe {
                        cache::render_dirty_rects(surface, commands, &dirty);
                    }
                    // Update dirty regions on screen.
                    let sdl_rects: Vec<SDL_Rect> = dirty
                        .iter()
                        .map(|r| SDL_Rect {
                            x: r.x,
                            y: r.y,
                            w: r.w,
                            h: r.h,
                        })
                        .collect();
                    unsafe {
                        SDL_UpdateWindowSurfaceRects(
                            window,
                            sdl_rects.as_ptr(),
                            sdl_rects.len() as libc::c_int,
                        );
                    }
                    crate::window::show_if_hidden();
                });
            });
            Ok(())
        })?,
    )?;

    // ── renderer.set_clip_rect ─────────────────────────────────────────────

    r.set(
        "set_clip_rect",
        lua.create_function(|_, (x, y, w, h): (i32, i32, i32, i32)| {
            with_cache(|c| c.push_set_clip(RenRect { x, y, w, h }));
            Ok(())
        })?,
    )?;

    // ── renderer.draw_rect ─────────────────────────────────────────────────

    r.set(
        "draw_rect",
        lua.create_function(|_, (x, y, w, h, color): (i32, i32, i32, i32, LuaTable)| {
            let color = table_to_color(&color)?;
            with_cache(|c| c.push_draw_rect(RenRect { x, y, w, h }, color));
            Ok(())
        })?,
    )?;

    // ── renderer.draw_text ─────────────────────────────────────────────────

    r.set(
        "draw_text",
        lua.create_function(
            |_,
             (font_tbl, text, x, y, color, opts): (
                LuaTable,
                String,
                f32,
                i32,
                LuaTable,
                Option<LuaTable>,
            )| {
                let color = table_to_color(&color)?;
                let tab_offset = opts
                    .as_ref()
                    .and_then(|t| t.get::<Option<f32>>("tab_offset").ok().flatten())
                    .unwrap_or(0.0);
                let fonts = extract_font_group(&LuaValue::Table(font_tbl))?;
                let new_x = CACHE.with(|cell| {
                    let mut borrow = cell.borrow_mut();
                    if borrow.is_none() {
                        *borrow = Some(RenCache::new());
                    }
                    borrow
                        .as_mut()
                        .unwrap()
                        .push_draw_text(fonts, text, x, y, color, tab_offset)
                });
                Ok(new_x)
            },
        )?,
    )?;

    // ── renderer.get_size ──────────────────────────────────────────────────

    r.set(
        "get_size",
        lua.create_function(|_, ()| Ok(crate::window::get_drawable_size()))?,
    )?;

    // ── renderer.show_debug ────────────────────────────────────────────────

    r.set(
        "show_debug",
        lua.create_function(|_, enable: bool| {
            with_cache(|c| c.show_debug = enable);
            Ok(())
        })?,
    )?;

    Ok(r)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn table_to_color(t: &LuaTable) -> LuaResult<RenColor> {
    Ok(RenColor {
        r: t.raw_get::<u8>(1).unwrap_or(0),
        g: t.raw_get::<u8>(2).unwrap_or(0),
        b: t.raw_get::<u8>(3).unwrap_or(0),
        a: t.raw_get::<u8>(4).unwrap_or(255),
    })
}

/// Wrap a RenFont in a Lua table {[1] = ud} with the given metatable.
fn wrap_font(lua: &Lua, rf: RenFont, meta: LuaTable) -> LuaResult<LuaTable> {
    let t = lua.create_table()?;
    t.raw_set(1, rf)?;
    t.set_metatable(Some(meta))?;
    Ok(t)
}

/// Extract the inner RenFont userdata from a font table {[1] = RenFont_ud}.
fn ud_from_font_tbl(t: &LuaTable) -> LuaResult<LuaAnyUserData> {
    t.raw_get::<LuaAnyUserData>(1)
}

/// Extract all FontRefs from a font table (which may hold a group-font RenFont).
fn extract_font_group(val: &LuaValue) -> LuaResult<Vec<FontRef>> {
    match val {
        LuaValue::Table(t) => {
            let ud = ud_from_font_tbl(t)?;
            let rf = ud.borrow::<RenFont>()?;
            Ok(rf.0.clone())
        }
        _ => Err(LuaError::runtime("expected font table")),
    }
}
