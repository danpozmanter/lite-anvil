#![allow(dead_code)]
use std::sync::atomic::{AtomicBool, Ordering};

use once_cell::sync::Lazy;
use parking_lot::Mutex;

/// Global application state flags, accessible from both the main loop
/// and views.
static STATE: Lazy<Mutex<FrameFlags>> = Lazy::new(|| Mutex::new(FrameFlags::default()));

/// Frame-level flags shared between subsystems.
#[derive(Debug, Default)]
pub struct FrameFlags {
    pub redraw: bool,
    pub quit_request: bool,
    pub restart_request: bool,
    pub frame_start: f64,
    pub blink_start: f64,
    pub blink_timer: f64,
}

/// Read the current frame flags.
pub fn flags() -> FrameFlags {
    let s = STATE.lock();
    FrameFlags {
        redraw: s.redraw,
        quit_request: s.quit_request,
        restart_request: s.restart_request,
        frame_start: s.frame_start,
        blink_start: s.blink_start,
        blink_timer: s.blink_timer,
    }
}

/// Update frame flags.
pub fn set_frame_start(t: f64) {
    STATE.lock().frame_start = t;
}

pub fn set_redraw(v: bool) {
    STATE.lock().redraw = v;
}

pub fn set_blink_start(t: f64) {
    let mut s = STATE.lock();
    s.blink_start = t;
    s.blink_timer = t;
}

pub fn request_quit() {
    STATE.lock().quit_request = true;
}

pub fn request_restart() {
    STATE.lock().restart_request = true;
}

// ── Clip rect stack ─────────────────────────────────────────────────────────

static CLIP_STACK: Lazy<Mutex<Vec<[f64; 4]>>> = Lazy::new(|| Mutex::new(Vec::new()));

/// Initialize the clip stack with a full-screen rect.
pub fn clip_init(w: f64, h: f64) {
    let mut stack = CLIP_STACK.lock();
    stack.clear();
    stack.push([0.0, 0.0, w, h]);
    #[cfg(feature = "sdl")]
    crate::renderer::with_cache(|c| {
        c.push_set_clip(crate::renderer::RenRect {
            x: 0, y: 0, w: w as i32, h: h as i32,
        });
    });
}

/// Push an intersected clip rect and apply to renderer.
pub fn clip_push(x: f64, y: f64, w: f64, h: f64) {
    let mut stack = CLIP_STACK.lock();
    let [x2, y2, w2, h2] = stack.last().copied().unwrap_or([0.0, 0.0, 0.0, 0.0]);
    let nx = x.max(x2);
    let ny = y.max(y2);
    let nr = (x + w).min(x2 + w2);
    let nb = (y + h).min(y2 + h2);
    let nw = nr - nx;
    let nh = nb - ny;
    stack.push([nx, ny, nw, nh]);
    #[cfg(feature = "sdl")]
    crate::renderer::with_cache(|c| {
        c.push_set_clip(crate::renderer::RenRect {
            x: nx as i32, y: ny as i32, w: nw as i32, h: nh as i32,
        });
    });
}

/// Pop the top clip rect and restore the previous one.
pub fn clip_pop() {
    let mut stack = CLIP_STACK.lock();
    stack.pop();
    let [x, y, w, h] = stack.last().copied().unwrap_or([0.0, 0.0, 0.0, 0.0]);
    #[cfg(feature = "sdl")]
    crate::renderer::with_cache(|c| {
        c.push_set_clip(crate::renderer::RenRect {
            x: x as i32, y: y as i32, w: w as i32, h: h as i32,
        });
    });
}

/// Global quit signal (can be set from signal handlers).
static QUIT_SIGNALED: AtomicBool = AtomicBool::new(false);

pub fn signal_quit() {
    QUIT_SIGNALED.store(true, Ordering::Relaxed);
}

pub fn take_quit_signal() -> bool {
    QUIT_SIGNALED.swap(false, Ordering::Relaxed)
}
