mod cache;
mod fallback;
pub(crate) mod font;

pub(crate) use cache::{RenColor, RenRect, group_text_width};
pub(crate) use fallback::take_uncovered;
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
    fonts: std::sync::Arc<[FontRef]>,
    text: &str,
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
        let arena = &cache.text_arena;
        crate::window::with_window_surface(|surface, window| {
            // SAFETY: surface is valid for this call; we're on the main thread.
            unsafe {
                cache::render_dirty_rects(surface, commands, arena, &dirty);
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

/// Drop per-window caches that are cheap to rebuild on next draw.
/// Called when the window is occluded/hidden so we don't hold onto
/// megabytes of glyph bitmaps and render-cache command buffers while
/// the compositor isn't showing our frames.
pub fn drop_caches() {
    CACHE.with(|c| {
        *c.borrow_mut() = None;
    });
    font::clear_glyph_caches();
}

/// macOS memory-pressure level.  `Some(0)` normal, `Some(1)` warn,
/// `Some(2)` critical, `None` when the sysctl isn't available (non-
/// macOS or the node doesn't exist on the running kernel).
#[cfg(target_os = "macos")]
pub fn macos_memory_pressure_level() -> Option<u32> {
    use std::ffi::CString;
    let name = CString::new("kern.memorystatus_vm_pressure_level").ok()?;
    let mut value: u32 = 0;
    let mut size: libc::size_t = std::mem::size_of::<u32>();
    // SAFETY: `name` is a NUL-terminated C string we just created;
    // `value` and `size` are valid for reads/writes of sizeof(u32).
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            &mut value as *mut u32 as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc == 0 { Some(value) } else { None }
}

#[cfg(not(target_os = "macos"))]
pub fn macos_memory_pressure_level() -> Option<u32> {
    None
}
