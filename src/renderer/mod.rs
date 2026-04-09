mod cache;
mod font;

pub(crate) use cache::{RenColor, RenRect};
pub(crate) use font::{Antialiasing, FontInner, FontRef, Hinting};

use cache::RenCache;
use sdl3_sys::everything::*;

// ── Thread-local renderer state ───────────────────────────────────────────────

thread_local! {
    static CACHE: std::cell::RefCell<Option<RenCache>> =
        const { std::cell::RefCell::new(None) };
}

pub(crate) fn with_cache<F: FnOnce(&mut RenCache)>(f: F) {
    CACHE.with(|c| {
        let mut borrow = c.borrow_mut();
        if borrow.is_none() {
            *borrow = Some(RenCache::new());
        }
        f(borrow.as_mut().unwrap());
    });
}

/// Push a draw_text command directly to the thread-local cache.
/// Returns the new x position after the text.
#[allow(non_snake_case)]
pub fn CACHE_DRAW_TEXT(
    fonts: Vec<FontRef>,
    text: String,
    x: f32,
    y: i32,
    color: RenColor,
    tab_offset: f32,
) -> f32 {
    CACHE.with(|cell| {
        let mut borrow = cell.borrow_mut();
        if borrow.is_none() {
            *borrow = Some(RenCache::new());
        }
        borrow
            .as_mut()
            .unwrap()
            .push_draw_text(fonts, text, x, y, color, tab_offset)
    })
}

/// Native begin_frame: initialize the render cache for a new frame.
pub fn native_begin_frame() {
    let (w, h) = crate::window::get_drawable_size();
    with_cache(|c| {
        if crate::window::take_needs_invalidate() {
            c.invalidate();
        }
        c.begin_frame(w, h);
    });
}

/// Native end_frame: compute dirty rects and render to the SDL surface.
pub fn native_end_frame() {
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
            let sdl_rects: Vec<SDL_Rect> = dirty
                .iter()
                .map(|r| SDL_Rect {
                    x: r.x,
                    y: r.y,
                    w: r.w,
                    h: r.h,
                })
                .collect();
            // SAFETY: window is valid; sdl_rects is a valid slice.
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
}

/// Drop the renderer cache, releasing FontRef arcs held by previous draw commands.
#[allow(dead_code)]
pub fn reset_cache() {
    CACHE.with(|c| {
        *c.borrow_mut() = None;
    });
}

