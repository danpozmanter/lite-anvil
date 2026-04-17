use anyhow::{Context, Result};
use sdl3_sys::everything::*;
use std::cell::RefCell;
use std::ffi::{CStr, CString};

/// Numpad keys when NumLock is off, indexed by (Scancode - Kp1).
/// Covers Kp1..Kp9, Kp0, KpPeriod (11 entries, matching SDL scancode layout).
static NUMPAD: [&str; 11] = [
    "end", "down", "pagedown", "left", "", "right", "home", "up", "pageup", "ins", "delete",
];

const DEFAULT_WINDOW_SCALE_NUM: i32 = 4;
const DEFAULT_WINDOW_SCALE_DEN: i32 = 5;
const DEFAULT_WINDOW_MIN_W: i32 = 400;
const DEFAULT_WINDOW_MIN_H: i32 = 300;
const MAX_STARTUP_BACKBUFFER_PIXELS: i64 = 2560 * 1440;

struct SdlWindow {
    raw: *mut SDL_Window,
    /// Pixel-to-point ratio for HiDPI content scaling.
    scale_x: f32,
    scale_y: f32,
}

impl SdlWindow {
    fn update_scale(&mut self) {
        let mut lw = 0i32;
        let mut lh = 0i32;
        let mut pw = 0i32;
        let mut ph = 0i32;
        // SAFETY: self.raw is a valid SDL_Window pointer owned by this struct.
        unsafe {
            SDL_GetWindowSize(self.raw, &mut lw, &mut lh);
            SDL_GetWindowSizeInPixels(self.raw, &mut pw, &mut ph);
        }
        self.scale_x = if lw > 0 { pw as f32 / lw as f32 } else { 1.0 };
        self.scale_y = if lh > 0 { ph as f32 / lh as f32 } else { 1.0 };
    }

    fn flags(&self) -> SDL_WindowFlags {
        // SAFETY: self.raw is a valid SDL_Window pointer.
        unsafe { SDL_GetWindowFlags(self.raw) }
    }

    fn logical_size_from_pixels(&self, pixel_w: i32, pixel_h: i32) -> (i32, i32) {
        let logical_w = if self.scale_x > 0.0 {
            (pixel_w as f32 / self.scale_x).round() as i32
        } else {
            pixel_w
        };
        let logical_h = if self.scale_y > 0.0 {
            (pixel_h as f32 / self.scale_y).round() as i32
        } else {
            pixel_h
        };
        (logical_w.max(1), logical_h.max(1))
    }

    fn logical_position_from_pixels(&self, pixel_x: i32, pixel_y: i32) -> (i32, i32) {
        let logical_x = if self.scale_x > 0.0 {
            (pixel_x as f32 / self.scale_x).round() as i32
        } else {
            pixel_x
        };
        let logical_y = if self.scale_y > 0.0 {
            (pixel_y as f32 / self.scale_y).round() as i32
        } else {
            pixel_y
        };
        (logical_x, logical_y)
    }
}

struct SdlState {
    window: Option<SdlWindow>,
    /// Active mouse cursor — must be kept alive while in use.
    cursor: Option<*mut SDL_Cursor>,
    /// Buffered event saved during mouse-motion coalescing or focus-gained drain.
    pending_event: Option<SDL_Event>,
    /// Window kept alive across restarts.
    persistent: bool,
    /// Set when SDL_EVENT_WINDOW_EXPOSED is received; renderer clears it in begin_frame
    /// and forces a full cache invalidation so the surface content is redrawn.
    needs_invalidate: bool,
}

thread_local! {
    static SDL: RefCell<Option<SdlState>> = const { RefCell::new(None) };
}

/// Returns a human-readable SDL3 error string.
fn sdl_error() -> String {
    // SAFETY: SDL_GetError returns a valid C string or null.
    unsafe {
        let ptr = SDL_GetError();
        if ptr.is_null() {
            return String::new();
        }
        CStr::from_ptr(ptr).to_string_lossy().into_owned()
    }
}

// App metadata passed to SDL_SetAppMetadata in `init`. Defaults match
// Lite-Anvil; Nano-Anvil overrides these via `set_app_metadata` before
// calling `init` so the Linux taskbar matches `nano-anvil.desktop`
// (StartupWMClass=nano-anvil, Icon=nano-anvil) instead of falling back
// to the Lite-Anvil entry.
thread_local! {
    static APP_NAME: std::cell::RefCell<std::ffi::CString> =
        std::cell::RefCell::new(
            std::ffi::CString::new("Lite-Anvil").expect("static app name"),
        );
    static APP_IDENTIFIER: std::cell::RefCell<std::ffi::CString> =
        std::cell::RefCell::new(
            std::ffi::CString::new("com.lite-anvil.LiteAnvil").expect("static app id"),
        );
}

/// Override the `(name, identifier)` pair passed to SDL's app metadata.
/// Must be called before `init`. The identifier ends up as the Wayland
/// `app_id` and influences X11 `WM_CLASS`, so it should match the
/// `StartupWMClass` field of the app's `.desktop` file for the right
/// taskbar icon to be picked.
pub fn set_app_metadata(name: &str, identifier: &str) {
    if let Ok(n) = std::ffi::CString::new(name) {
        APP_NAME.with(|cell| *cell.borrow_mut() = n);
    }
    if let Ok(id) = std::ffi::CString::new(identifier) {
        APP_IDENTIFIER.with(|cell| *cell.borrow_mut() = id);
    }
}

/// Initialise SDL3 video subsystem. Must be called once on the main thread before
/// the editor starts.
pub fn init() -> Result<()> {
    // Nudge SDL toward a no-GPU presentation path. Our static SDL3 build
    // still has OpenGL / Vulkan / Wayland compiled in because sdl3-sys
    // 0.6 doesn't expose Cargo features to disable them at CMake time;
    // these hints steer it away from them at runtime so the editor
    // behaves the same on a Nix / Arch / Fedora source build as on our
    // old hand-crafted "no-GL" tarballs.
    //
    //  * FRAMEBUFFER_ACCELERATION=0 — the important one. By default on
    //    Linux, `SDL_GetWindowSurface` silently spins up an SDL_Renderer
    //    (OpenGL) to present via a texture, which dlopens libGL +
    //    libGLX_nvidia + libnvidia-glcore + friends and balloons RSS
    //    from ~18 MB to ~70 MB. Setting this hint tells SDL to use the
    //    plain X11 SHM framebuffer path with no GL involvement at all.
    //  * VIDEO_DRIVER=x11,wayland — prefer X11 (which can present via
    //    MIT-SHM with no GL) on Linux, only falling back to Wayland
    //    when X11 is absent. Windows and macOS ignore this hint.
    //  * RENDER_DRIVER=software — belt-and-braces: if anything in the
    //    stack does spin up SDL_Renderer, force the CPU backend.
    unsafe {
        SDL_SetHint(
            c"SDL_FRAMEBUFFER_ACCELERATION".as_ptr(),
            c"0".as_ptr(),
        );
        SDL_SetHint(
            c"SDL_VIDEO_DRIVER".as_ptr(),
            c"x11,wayland".as_ptr(),
        );
        SDL_SetHint(
            c"SDL_RENDER_DRIVER".as_ptr(),
            c"software".as_ptr(),
        );
    }

    APP_NAME.with(|name| {
        APP_IDENTIFIER.with(|ident| {
            // SDL copies these strings internally, so the borrow only needs
            // to outlive the call itself.
            unsafe {
                SDL_SetAppMetadata(
                    name.borrow().as_ptr(),
                    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr().cast(),
                    ident.borrow().as_ptr(),
                );
            }
        });
    });
    let ok = unsafe { SDL_Init(SDL_INIT_VIDEO) };
    if !ok {
        return Err(anyhow::anyhow!("SDL3 init failed: {}", sdl_error()));
    }
    SDL.with(|s| {
        *s.borrow_mut() = Some(SdlState {
            window: None,
            cursor: None,
            pending_event: None,
            persistent: false,
            needs_invalidate: false,
        });
    });
    Ok(())
}

/// Tear down SDL3. Called after the main loop exits entirely.
pub fn shutdown() {
    SDL.with(|s| {
        if let Some(mut state) = s.borrow_mut().take() {
            if let Some(w) = state.window.take() {
                // SAFETY: w.raw is valid; ownership is consumed here.
                unsafe { SDL_DestroyWindow(w.raw) };
            }
            if let Some(c) = state.cursor.take() {
                // SAFETY: cursor is valid; ownership is consumed here.
                unsafe { SDL_DestroyCursor(c) };
            }
        }
    });
    // SAFETY: Called once at program exit after all SDL resources are freed.
    unsafe { SDL_Quit() };
}

/// On restart: drop the window unless it was marked persistent.
#[allow(dead_code)]
pub fn prepare_restart() {
    SDL.with(|s| {
        if let Some(ref mut state) = *s.borrow_mut() {
            if !state.persistent {
                if let Some(w) = state.window.take() {
                    // SAFETY: w.raw is valid; ownership is consumed here.
                    unsafe { SDL_DestroyWindow(w.raw) };
                }
            }
        }
    });
}

// Bytes of the application icon used by `set_window_icon`. Defaults to
// the Lite-Anvil icon; Nano-Anvil overrides it at startup via
// `set_app_icon_bytes` before `new_window` is called.
thread_local! {
    static APP_ICON_BYTES: std::cell::RefCell<&'static [u8]> = const {
        std::cell::RefCell::new(include_bytes!("../../resources/icons/lite-anvil.png"))
    };
}

/// Replace the PNG used as the SDL window icon for the lifetime of this
/// process. Pass the bytes of a decoded PNG (typically via
/// `include_bytes!`); must be called before the first `new_window`.
pub fn set_app_icon_bytes(bytes: &'static [u8]) {
    APP_ICON_BYTES.with(|cell| *cell.borrow_mut() = bytes);
}

/// Decode the embedded PNG and set it as the SDL window icon.
fn set_window_icon(win: *mut SDL_Window) {
    let icon_bytes: &[u8] = APP_ICON_BYTES.with(|cell| *cell.borrow());

    let decoder = png::Decoder::new(std::io::Cursor::new(icon_bytes));
    let mut reader = match decoder.read_info() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("icon: failed to read PNG info: {e}");
            return;
        }
    };

    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = match reader.next_frame(&mut buf) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("icon: failed to decode PNG: {e}");
            return;
        }
    };

    // Convert to RGBA8 so SDL gets a consistent format.
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => buf[..info.buffer_size()].to_vec(),
        png::ColorType::Rgb => buf[..info.buffer_size()]
            .chunks(3)
            .flat_map(|c| [c[0], c[1], c[2], 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => buf[..info.buffer_size()]
            .chunks(2)
            .flat_map(|c| [c[0], c[0], c[0], c[1]])
            .collect(),
        png::ColorType::Grayscale => buf[..info.buffer_size()]
            .iter()
            .flat_map(|&v| [v, v, v, 255])
            .collect(),
        _ => {
            eprintln!("icon: unsupported PNG color type {:?}", info.color_type);
            return;
        }
    };

    let width = info.width as i32;
    let height = info.height as i32;
    let pitch = width * 4;

    // SAFETY: rgba buffer outlives the surface; win is a valid SDL_Window pointer.
    unsafe {
        let surface = SDL_CreateSurfaceFrom(
            width,
            height,
            SDL_PIXELFORMAT_RGBA32,
            rgba.as_ptr() as *mut std::ffi::c_void,
            pitch,
        );
        if !surface.is_null() {
            SDL_SetWindowIcon(win, surface);
            SDL_DestroySurface(surface);
        }
    }
}

fn clamp_startup_backbuffer(win: &mut SdlWindow) {
    let mut pw = 0i32;
    let mut ph = 0i32;
    // SAFETY: win.raw is a valid SDL_Window pointer.
    unsafe {
        SDL_GetWindowSizeInPixels(win.raw, &mut pw, &mut ph);
    }
    let area = i64::from(pw.max(1)) * i64::from(ph.max(1));
    if area <= MAX_STARTUP_BACKBUFFER_PIXELS {
        return;
    }

    let scale = (MAX_STARTUP_BACKBUFFER_PIXELS as f64 / area as f64).sqrt();
    let target_pw = ((pw as f64) * scale).floor() as i32;
    let target_ph = ((ph as f64) * scale).floor() as i32;
    let (logical_w, logical_h) = win.logical_size_from_pixels(
        target_pw.max(DEFAULT_WINDOW_MIN_W),
        target_ph.max(DEFAULT_WINDOW_MIN_H),
    );
    // SAFETY: win.raw is a valid SDL_Window pointer.
    unsafe {
        SDL_SetWindowSize(win.raw, logical_w, logical_h);
    }
    win.update_scale();
}

/// Create the SDL3 window and store it. Called from `renwindow.create()`.
pub fn create_window(title: &str) -> Result<()> {
    SDL.with(|s| -> Result<()> {
        let mut guard = s.borrow_mut();
        let state = guard.as_mut().context("SDL not initialised")?;

        let mut bounds = SDL_Rect {
            x: 0,
            y: 0,
            w: 1280,
            h: 800,
        };
        // SAFETY: SDL is initialized; display query functions are safe to call.
        unsafe {
            let disp = SDL_GetPrimaryDisplay();
            if !SDL_GetDisplayUsableBounds(disp, &mut bounds) {
                SDL_GetDisplayBounds(disp, &mut bounds);
            }
        }
        let width = (bounds.w * DEFAULT_WINDOW_SCALE_NUM / DEFAULT_WINDOW_SCALE_DEN)
            .max(DEFAULT_WINDOW_MIN_W);
        let height = (bounds.h * DEFAULT_WINDOW_SCALE_NUM / DEFAULT_WINDOW_SCALE_DEN)
            .max(DEFAULT_WINDOW_MIN_H);

        let title_cstr = CString::new(title).unwrap_or_default();
        let flags = SDL_WINDOW_RESIZABLE | SDL_WINDOW_HIGH_PIXEL_DENSITY | SDL_WINDOW_HIDDEN;
        // SAFETY: SDL is initialized; title_cstr is a valid C string.
        let win = unsafe { SDL_CreateWindow(title_cstr.as_ptr(), width, height, flags) };
        if win.is_null() {
            return Err(anyhow::anyhow!("window creation failed: {}", sdl_error()));
        }

        // Set the application icon.
        set_window_icon(win);

        let mut lw = 0i32;
        let mut lh = 0i32;
        let mut pw = 0i32;
        let mut ph = 0i32;
        // SAFETY: win was just successfully created and is non-null.
        unsafe {
            SDL_GetWindowSize(win, &mut lw, &mut lh);
            SDL_GetWindowSizeInPixels(win, &mut pw, &mut ph);
        }
        let scale_x = if lw > 0 { pw as f32 / lw as f32 } else { 1.0 };
        let scale_y = if lh > 0 { ph as f32 / lh as f32 } else { 1.0 };

        let mut window = SdlWindow {
            raw: win,
            scale_x,
            scale_y,
        };
        clamp_startup_backbuffer(&mut window);
        state.window = Some(window);
        Ok(())
    })
}

/// Returns true if a persistent window from a previous restart is available.
pub fn restore_window() -> bool {
    SDL.with(|s| {
        s.borrow()
            .as_ref()
            .map(|st| st.persistent && st.window.is_some())
            .unwrap_or(false)
    })
}

/// Mark the current window as persistent (survives restart).
pub fn set_persistent(p: bool) {
    SDL.with(|s| {
        if let Some(ref mut st) = *s.borrow_mut() {
            st.persistent = p;
        }
    });
}

// ── Window property accessors ─────────────────────────────────────────────────

pub fn show_window() {
    SDL.with(|s| {
        if let Some(ref mut st) = *s.borrow_mut() {
            if let Some(ref w) = st.window {
                // SAFETY: w.raw is a valid SDL_Window pointer.
                unsafe { SDL_ShowWindow(w.raw) };
            }
        }
    });
}

/// Show the window if it was created hidden and has not yet been shown.
/// Called after the first rendered frame to avoid startup flicker.
pub fn show_if_hidden() {
    SDL.with(|s| {
        if let Some(ref st) = *s.borrow() {
            if let Some(ref w) = st.window {
                // SAFETY: w.raw is a valid SDL_Window pointer.
                if (unsafe { SDL_GetWindowFlags(w.raw) } & SDL_WINDOW_HIDDEN).0 != 0 {
                    unsafe { SDL_ShowWindow(w.raw) };
                }
            }
        }
    });
}

/// Returns `(pw, ph, x, y)` — physical pixels and scaled screen position.
pub fn get_window_size() -> (i32, i32, i32, i32) {
    SDL.with(|s| {
        s.borrow()
            .as_ref()
            .and_then(|st| st.window.as_ref())
            .map(|w| {
                let mut pw = 0i32;
                let mut ph = 0i32;
                let mut x = 0i32;
                let mut y = 0i32;
                // SAFETY: w.raw is a valid SDL_Window pointer.
                unsafe {
                    SDL_GetWindowSizeInPixels(w.raw, &mut pw, &mut ph);
                    SDL_GetWindowPosition(w.raw, &mut x, &mut y);
                }
                (
                    pw,
                    ph,
                    (x as f32 * w.scale_x).round() as i32,
                    (y as f32 * w.scale_y).round() as i32,
                )
            })
            .unwrap_or((800, 600, 0, 0))
    })
}

/// HiDPI scale factor (pixel size / logical window size).
/// Enable SDL text input events. Must be called after window creation.
pub fn start_text_input() {
    SDL.with(|s| {
        if let Some(ref st) = *s.borrow() {
            if let Some(ref w) = st.window {
                // SAFETY: w.raw is a valid SDL_Window pointer.
                unsafe { SDL_StartTextInput(w.raw) };
            }
        }
    });
}

/// Get text from the system clipboard.
pub fn get_clipboard_text() -> Option<String> {
    // SAFETY: SDL_GetClipboardText returns a valid C string that must be freed.
    let ptr = unsafe { SDL_GetClipboardText() };
    if ptr.is_null() {
        return None;
    }
    let text = unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned();
    unsafe { sdl3_sys::everything::SDL_free(ptr as *mut std::ffi::c_void) };
    if text.is_empty() { None } else { Some(text) }
}

/// Set text to the system clipboard.
pub fn set_clipboard_text(text: &str) {
    if let Ok(cstr) = CString::new(text) {
        // SAFETY: cstr is a valid C string.
        unsafe { SDL_SetClipboardText(cstr.as_ptr()) };
    }
}

pub fn get_display_scale() -> f64 {
    SDL.with(|s| {
        s.borrow()
            .as_ref()
            .and_then(|st| st.window.as_ref())
            .map(|w| w.scale_x as f64)
            .unwrap_or(1.0)
    })
}

pub fn set_window_size(w: i32, h: i32, x: i32, y: i32) {
    SDL.with(|s| {
        if let Some(ref mut st) = *s.borrow_mut() {
            if let Some(ref mut win) = st.window {
                let (logical_w, logical_h) = win.logical_size_from_pixels(w.max(1), h.max(1));
                let (logical_x, logical_y) = if x != -1 || y != -1 {
                    win.logical_position_from_pixels(x, y)
                } else {
                    (x, y)
                };
                // SAFETY: win.raw is a valid SDL_Window pointer.
                unsafe {
                    SDL_SetWindowSize(win.raw, logical_w, logical_h);
                    if x != -1 || y != -1 {
                        SDL_SetWindowPosition(win.raw, logical_x, logical_y);
                    }
                    // Wait for the compositor to process the resize before the
                    // first draw, otherwise the renderer may present a frame at
                    // the old (creation) size, causing a visible size mismatch.
                    SDL_SyncWindow(win.raw);
                }
                win.update_scale();
            }
        }
    });
}

pub fn set_window_title(title: &str) {
    SDL.with(|s| {
        if let Some(ref mut st) = *s.borrow_mut() {
            if let Some(ref w) = st.window {
                let cstr = CString::new(title).unwrap_or_default();
                // SAFETY: w.raw is a valid SDL_Window; cstr is a valid C string.
                unsafe { SDL_SetWindowTitle(w.raw, cstr.as_ptr()) };
            }
        }
    });
}

/// `mode`: "normal" | "maximized" | "fullscreen"
pub fn set_window_mode(mode: &str) {
    SDL.with(|s| {
        if let Some(ref mut st) = *s.borrow_mut() {
            if let Some(ref w) = st.window {
                // SAFETY: w.raw is a valid SDL_Window pointer.
                match mode {
                    "maximized" => unsafe {
                        SDL_MaximizeWindow(w.raw);
                    },
                    "fullscreen" => unsafe {
                        SDL_SetWindowFullscreen(w.raw, true);
                    },
                    _ => unsafe {
                        let flags = SDL_GetWindowFlags(w.raw);
                        if (flags & SDL_WINDOW_FULLSCREEN).0 != 0 {
                            SDL_SetWindowFullscreen(w.raw, false);
                        } else {
                            SDL_RestoreWindow(w.raw);
                        }
                    },
                }
            }
        }
    });
}

/// Returns "normal" | "maximized" | "fullscreen" | "minimized".
pub fn get_window_mode() -> &'static str {
    SDL.with(|s| {
        s.borrow()
            .as_ref()
            .and_then(|st| st.window.as_ref())
            .map(|w| {
                let flags = w.flags();
                if (flags & SDL_WINDOW_FULLSCREEN).0 != 0 {
                    "fullscreen"
                } else if (flags & SDL_WINDOW_MAXIMIZED).0 != 0 {
                    "maximized"
                } else if (flags & SDL_WINDOW_MINIMIZED).0 != 0 {
                    "minimized"
                } else {
                    "normal"
                }
            })
            .unwrap_or("normal")
    })
}

pub fn set_window_bordered(bordered: bool) {
    SDL.with(|s| {
        if let Some(ref mut st) = *s.borrow_mut() {
            if let Some(ref w) = st.window {
                // SAFETY: w.raw is a valid SDL_Window pointer.
                unsafe { SDL_SetWindowBordered(w.raw, bordered) };
            }
        }
    });
}

pub fn window_has_focus() -> bool {
    SDL.with(|s| {
        s.borrow()
            .as_ref()
            .and_then(|st| st.window.as_ref())
            .map(|w| (w.flags() & SDL_WINDOW_INPUT_FOCUS).0 != 0)
            .unwrap_or(false)
    })
}

/// Returns `(w, h)` of the primary display in pixels.
pub fn get_screen_size() -> (i32, i32) {
    SDL.with(|s| {
        s.borrow()
            .as_ref()
            .map(|st| {
                let mut bounds = SDL_Rect {
                    x: 0,
                    y: 0,
                    w: 1920,
                    h: 1080,
                };
                // SAFETY: SDL is initialized; display query is safe to call.
                unsafe {
                    let disp = SDL_GetPrimaryDisplay();
                    SDL_GetDisplayBounds(disp, &mut bounds);
                }
                let (scale_x, scale_y) = st
                    .window
                    .as_ref()
                    .map(|w| (w.scale_x, w.scale_y))
                    .unwrap_or((1.0, 1.0));
                (
                    (bounds.w as f32 * scale_x).round() as i32,
                    (bounds.h as f32 * scale_y).round() as i32,
                )
            })
            .unwrap_or((1920, 1080))
    })
}

/// Map a cursor name to an SDL3 system cursor and activate it.
pub fn set_cursor(name: &str) {
    let id = match name {
        "ibeam" => SDL_SystemCursor::TEXT,
        "hand" => SDL_SystemCursor::POINTER,
        "sizeh" => SDL_SystemCursor::EW_RESIZE,
        "sizev" => SDL_SystemCursor::NS_RESIZE,
        "sizeall" => SDL_SystemCursor::MOVE,
        _ => SDL_SystemCursor::DEFAULT,
    };
    SDL.with(|s| {
        if let Some(ref mut st) = *s.borrow_mut() {
            // SAFETY: SDL is initialized; cursor lifecycle managed by SdlState.
            unsafe {
                let new_cur = SDL_CreateSystemCursor(id);
                if !new_cur.is_null() {
                    SDL_SetCursor(new_cur);
                    if let Some(old) = st.cursor.replace(new_cur) {
                        SDL_DestroyCursor(old);
                    }
                }
            }
        }
    });
}

/// Force a full repaint on the next frame by setting the invalidation flag.
pub fn force_invalidate() {
    SDL.with(|s| {
        if let Some(st) = s.borrow_mut().as_mut() {
            st.needs_invalidate = true;
        }
    });
}

/// Returns true and clears the flag if an expose event was received since the last call.
/// Called by the renderer's begin_frame to force full cache invalidation after expose.
pub fn take_needs_invalidate() -> bool {
    SDL.with(|s| {
        s.borrow_mut()
            .as_mut()
            .map(|st| {
                let v = st.needs_invalidate;
                st.needs_invalidate = false;
                v
            })
            .unwrap_or(false)
    })
}

/// Returns the raw SDL_Window pointer (null if no window exists).
/// Used by api/mod.rs for text-input functions that require a window arg.
pub fn get_raw_window() -> *mut SDL_Window {
    SDL.with(|s| {
        s.borrow()
            .as_ref()
            .and_then(|st| st.window.as_ref())
            .map(|w| w.raw)
            .unwrap_or(std::ptr::null_mut())
    })
}

// ── Event polling ─────────────────────────────────────────────────────────────

/// Result of translating a pending SDL event.
pub enum PollResult {
    Empty,
    Skip,
    Event(Vec<EventVal>),
}

/// A typed value returned from poll_event.
pub enum EventVal {
    Str(&'static str),
    String(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

// ── Renderer surface access ────────────────────────────────────────────────────

/// Return the drawable pixel size of the window (for renderer begin_frame).
pub fn get_drawable_size() -> (i32, i32) {
    SDL.with(|s| {
        s.borrow()
            .as_ref()
            .and_then(|st| st.window.as_ref())
            .map(|w| {
                let mut pw = 0i32;
                let mut ph = 0i32;
                // SAFETY: w.raw is a valid SDL_Window pointer.
                unsafe { SDL_GetWindowSizeInPixels(w.raw, &mut pw, &mut ph) };
                (pw, ph)
            })
            .unwrap_or((800, 600))
    })
}

/// Call `f(surface_ptr, window_ptr)` with the raw SDL3 window surface.
///
/// SAFETY: The surface pointer is valid only for the duration of `f`.
/// Both pointers are valid because we borrow SDL state for the call duration.
pub fn with_window_surface<F>(f: F)
where
    F: FnOnce(*mut SDL_Surface, *mut SDL_Window),
{
    SDL.with(|s| {
        let guard = s.borrow();
        if let Some(ref st) = *guard {
            if let Some(ref w) = st.window {
                let raw_win = w.raw;
                // SAFETY: raw_win is valid; SDL_GetWindowSurface is safe to call.
                let surface = unsafe { SDL_GetWindowSurface(raw_win) };
                if !surface.is_null() {
                    f(surface, raw_win);
                }
            }
        }
    });
}

/// Bring the window to the foreground and give it input focus.
pub fn raise_window() {
    SDL.with(|s| {
        let guard = s.borrow();
        if let Some(ref st) = *guard {
            if let Some(ref w) = st.window {
                // SAFETY: w.raw is a valid SDL_Window pointer.
                unsafe { SDL_RaiseWindow(w.raw) };
            }
        }
    });
}

/// Push a dummy SDL user event to wake up a blocked `wait_event` call.
/// Safe to call from any thread — SDL_PushEvent is documented as thread-safe.
pub fn push_wakeup_event() {
    // SAFETY: SDL_PushEvent is thread-safe in SDL3.
    unsafe {
        let mut e = SDL_Event::default();
        e.r#type = SDL_EVENT_USER.0;
        SDL_PushEvent(&mut e);
    }
}

/// Poll one raw event (from pending buffer or SDL).
fn poll_raw(state: &mut SdlState) -> Option<SDL_Event> {
    if let Some(e) = state.pending_event.take() {
        return Some(e);
    }
    let mut e = SDL_Event::default();
    // SAFETY: SDL is initialized; e is a valid mutable SDL_Event.
    if unsafe { SDL_PollEvent(&mut e) } {
        Some(e)
    } else {
        None
    }
}

pub fn poll_event() -> PollResult {
    SDL.with(|s| {
        let mut guard = s.borrow_mut();
        let Some(ref mut state) = *guard else {
            return PollResult::Empty;
        };
        let Some(event) = poll_raw(state) else {
            return PollResult::Empty;
        };
        translate_event(state, event)
    })
}

/// Poll one SDL event and return it as a native EditorEvent.
/// Returns None when the queue is empty. Skips unrecognized events.
pub fn poll_event_native() -> Option<crate::editor::event::EditorEvent> {
    SDL.with(|s| {
        let mut guard = s.borrow_mut();
        let state = guard.as_mut()?;
        loop {
            let event = poll_raw(state)?;
            if let Some(ed) = translate_event_native(state, event) {
                return Some(ed);
            }
        }
    })
}

/// Translate an SDL event to a native EditorEvent.
fn translate_event_native(
    state: &mut SdlState,
    event: SDL_Event,
) -> Option<crate::editor::event::EditorEvent> {
    use crate::editor::event::{EditorEvent, MouseButton as MB};

    let scale_x = state.window.as_ref().map(|w| w.scale_x).unwrap_or(1.0) as f64;
    let scale_y = state.window.as_ref().map(|w| w.scale_y).unwrap_or(1.0) as f64;

    let t = unsafe { event.r#type };

    if t == SDL_EVENT_QUIT || t == SDL_EVENT_WINDOW_CLOSE_REQUESTED {
        return Some(EditorEvent::Quit);
    }
    if t == SDL_EVENT_WINDOW_RESIZED {
        let (w, h) = unsafe { (event.window.data1, event.window.data2) };
        if let Some(ref mut win) = state.window {
            win.update_scale();
        }
        return Some(EditorEvent::Resized {
            w: w as f64,
            h: h as f64,
        });
    }
    if t == SDL_EVENT_WINDOW_EXPOSED {
        state.needs_invalidate = true;
        return Some(EditorEvent::Exposed);
    }
    if t == SDL_EVENT_WINDOW_FOCUS_GAINED {
        return Some(EditorEvent::FocusGained);
    }
    if t == SDL_EVENT_WINDOW_FOCUS_LOST {
        return Some(EditorEvent::FocusLost);
    }
    if t == SDL_EVENT_KEY_DOWN || t == SDL_EVENT_KEY_UP {
        let (scancode, keycode, keymod, repeat) = unsafe {
            (
                event.key.scancode,
                event.key.key,
                event.key.r#mod,
                event.key.repeat,
            )
        };
        let name = key_name(keycode, scancode, keymod)?;
        let modifiers = crate::editor::event::Modifiers {
            shift: (keymod.0 & SDL_KMOD_SHIFT.0) != 0,
            ctrl: (keymod.0 & SDL_KMOD_CTRL.0) != 0,
            alt: (keymod.0 & SDL_KMOD_ALT.0) != 0,
            gui: (keymod.0 & SDL_KMOD_GUI.0) != 0,
        };
        let _ = repeat;
        return if t == SDL_EVENT_KEY_DOWN {
            Some(EditorEvent::KeyPressed {
                key: name,
                modifiers,
            })
        } else {
            Some(EditorEvent::KeyReleased {
                key: name,
                modifiers,
            })
        };
    }
    if t == SDL_EVENT_TEXT_INPUT {
        let text_ptr = unsafe { event.text.text };
        if text_ptr.is_null() {
            return None;
        }
        let text = unsafe { CStr::from_ptr(text_ptr) }
            .to_string_lossy()
            .into_owned();
        return Some(EditorEvent::TextInput(text));
    }
    if t == SDL_EVENT_MOUSE_BUTTON_DOWN || t == SDL_EVENT_MOUSE_BUTTON_UP {
        let (btn, x, y) = unsafe { (event.button.button, event.button.x, event.button.y) };
        let button = match btn {
            1 => MB::Left,
            2 => MB::Middle,
            3 => MB::Right,
            4 => MB::X1,
            5 => MB::X2,
            _ => return None,
        };
        return if t == SDL_EVENT_MOUSE_BUTTON_DOWN {
            // SDL only carries keyboard modifiers on key events, so query the live state here.
            let keymod = unsafe { SDL_GetModState() };
            let modifiers = crate::editor::event::Modifiers {
                shift: (keymod.0 & SDL_KMOD_SHIFT.0) != 0,
                ctrl: (keymod.0 & SDL_KMOD_CTRL.0) != 0,
                alt: (keymod.0 & SDL_KMOD_ALT.0) != 0,
                gui: (keymod.0 & SDL_KMOD_GUI.0) != 0,
            };
            Some(EditorEvent::MousePressed {
                button,
                x: x as f64 * scale_x,
                y: y as f64 * scale_y,
                clicks: 1,
                modifiers,
            })
        } else {
            Some(EditorEvent::MouseReleased {
                button,
                x: x as f64 * scale_x,
                y: y as f64 * scale_y,
            })
        };
    }
    if t == SDL_EVENT_MOUSE_MOTION {
        let mut fx = unsafe { event.motion.x };
        let mut fy = unsafe { event.motion.y };
        let mut frx = unsafe { event.motion.xrel };
        let mut fry = unsafe { event.motion.yrel };
        loop {
            let mut next_e = SDL_Event::default();
            if !unsafe { SDL_PollEvent(&mut next_e) } {
                break;
            }
            if unsafe { next_e.r#type } == SDL_EVENT_MOUSE_MOTION.0 {
                fx = unsafe { next_e.motion.x };
                fy = unsafe { next_e.motion.y };
                frx += unsafe { next_e.motion.xrel };
                fry += unsafe { next_e.motion.yrel };
            } else {
                state.pending_event = Some(next_e);
                break;
            }
        }
        return Some(EditorEvent::MouseMoved {
            x: fx as f64 * scale_x,
            y: fy as f64 * scale_y,
            dx: frx as f64 * scale_x,
            dy: fry as f64 * scale_y,
        });
    }
    if t == SDL_EVENT_MOUSE_WHEEL {
        let (wx, wy, dir) = unsafe { (event.wheel.x, event.wheel.y, event.wheel.direction) };
        let (vy, vx) = if dir == SDL_MouseWheelDirection::FLIPPED {
            (-wy, -wx)
        } else {
            (wy, wx)
        };
        return Some(EditorEvent::MouseWheel {
            x: -vx as f64,
            y: vy as f64,
        });
    }
    if t == SDL_EVENT_DROP_FILE {
        let data_ptr = unsafe { event.drop.data };
        if data_ptr.is_null() {
            return None;
        }
        let filename = unsafe { CStr::from_ptr(data_ptr) }
            .to_string_lossy()
            .into_owned();
        return Some(EditorEvent::FileDropped(std::path::PathBuf::from(filename)));
    }
    // Window minimize/maximize/restore/mouseleft events.
    if t == SDL_EVENT_WINDOW_MOUSE_LEAVE {
        return Some(EditorEvent::MouseLeft);
    }
    None
}

/// Block until an event arrives or `timeout_secs` elapses.
/// Returns `true` if an event is now pending.
pub fn wait_event(timeout_secs: Option<f64>) -> bool {
    SDL.with(|s| {
        let mut guard = s.borrow_mut();
        let Some(ref mut state) = *guard else {
            return false;
        };
        if state.pending_event.is_some() {
            return true;
        }
        let mut e = SDL_Event::default();
        // SAFETY: SDL is initialized; e is a valid mutable SDL_Event.
        let got = match timeout_secs {
            Some(t) => {
                let ms = (t * 1000.0).clamp(1.0, i32::MAX as f64) as i32;
                unsafe { SDL_WaitEventTimeout(&mut e, ms) }
            }
            None => unsafe { SDL_WaitEvent(&mut e) },
        };
        if got {
            state.pending_event = Some(e);
        }
        got
    })
}

// SAFETY for all union accesses below: the event type is checked before accessing
// the corresponding union variant, matching SDL3's tagged-union contract.
fn translate_event(state: &mut SdlState, event: SDL_Event) -> PollResult {
    use EventVal::*;

    let scale_x = state.window.as_ref().map(|w| w.scale_x).unwrap_or(1.0);
    let scale_y = state.window.as_ref().map(|w| w.scale_y).unwrap_or(1.0);

    let t = unsafe { event.r#type };

    if t == SDL_EVENT_QUIT {
        return PollResult::Event(vec![Str("quit")]);
    }

    if t == SDL_EVENT_WINDOW_CLOSE_REQUESTED {
        return PollResult::Event(vec![Str("quit")]);
    }

    if t == SDL_EVENT_WINDOW_RESIZED {
        let (w, h) = unsafe { (event.window.data1, event.window.data2) };
        if let Some(ref mut win) = state.window {
            win.update_scale();
        }
        return PollResult::Event(vec![Str("resized"), Int(w as i64), Int(h as i64)]);
    }

    if t == SDL_EVENT_WINDOW_EXPOSED {
        state.needs_invalidate = true;
        return PollResult::Event(vec![Str("exposed")]);
    }
    if t == SDL_EVENT_WINDOW_MINIMIZED {
        return PollResult::Event(vec![Str("minimized")]);
    }
    if t == SDL_EVENT_WINDOW_MAXIMIZED {
        return PollResult::Event(vec![Str("maximized")]);
    }
    if t == SDL_EVENT_WINDOW_RESTORED {
        return PollResult::Event(vec![Str("restored")]);
    }
    if t == SDL_EVENT_WINDOW_MOUSE_LEAVE {
        return PollResult::Event(vec![Str("mouseleft")]);
    }
    if t == SDL_EVENT_WINDOW_FOCUS_LOST {
        return PollResult::Event(vec![Str("focuslost")]);
    }

    if t == SDL_EVENT_WINDOW_FOCUS_GAINED {
        return PollResult::Event(vec![Str("focusgained")]);
    }

    if t == SDL_EVENT_KEY_DOWN || t == SDL_EVENT_KEY_UP {
        let (scancode, keycode, keymod, repeat) = unsafe {
            (
                event.key.scancode,
                event.key.key,
                event.key.r#mod,
                event.key.repeat,
            )
        };
        let Some(name) = key_name(keycode, scancode, keymod) else {
            return PollResult::Skip;
        };
        if t == SDL_EVENT_KEY_DOWN {
            return PollResult::Event(vec![Str("keypressed"), String(name), Bool(repeat)]);
        } else {
            return PollResult::Event(vec![Str("keyreleased"), String(name)]);
        }
    }

    if t == SDL_EVENT_TEXT_INPUT {
        let text_ptr = unsafe { event.text.text };
        if text_ptr.is_null() {
            return PollResult::Skip;
        }
        let text = unsafe { CStr::from_ptr(text_ptr) }
            .to_string_lossy()
            .into_owned();
        return PollResult::Event(vec![Str("textinput"), String(text)]);
    }

    if t == SDL_EVENT_TEXT_EDITING {
        let (text_ptr, start, length) =
            unsafe { (event.edit.text, event.edit.start, event.edit.length) };
        let text = if text_ptr.is_null() {
            std::string::String::new()
        } else {
            unsafe { CStr::from_ptr(text_ptr) }
                .to_string_lossy()
                .into_owned()
        };
        return PollResult::Event(vec![
            Str("textediting"),
            String(text),
            Int(start as i64),
            Int(length as i64),
        ]);
    }

    if t == SDL_EVENT_MOUSE_BUTTON_DOWN || t == SDL_EVENT_MOUSE_BUTTON_UP {
        let (btn, x, y) = unsafe { (event.button.button, event.button.x, event.button.y) };
        let Some(btn_name) = button_name(btn) else {
            return PollResult::Skip;
        };
        if t == SDL_EVENT_MOUSE_BUTTON_DOWN {
            return PollResult::Event(vec![
                Str("mousepressed"),
                Str(btn_name),
                Float(x as f64 * scale_x as f64),
                Float(y as f64 * scale_y as f64),
                Int(1),
            ]);
        } else {
            return PollResult::Event(vec![
                Str("mousereleased"),
                Str(btn_name),
                Float(x as f64 * scale_x as f64),
                Float(y as f64 * scale_y as f64),
            ]);
        }
    }

    if t == SDL_EVENT_MOUSE_MOTION {
        // Coalesce consecutive MouseMotion events — keep only the latest position.
        let mut fx = unsafe { event.motion.x };
        let mut fy = unsafe { event.motion.y };
        let mut frx = unsafe { event.motion.xrel };
        let mut fry = unsafe { event.motion.yrel };
        loop {
            let mut next_e = SDL_Event::default();
            if !unsafe { SDL_PollEvent(&mut next_e) } {
                break;
            }
            if unsafe { next_e.r#type } == SDL_EVENT_MOUSE_MOTION.0 {
                fx = unsafe { next_e.motion.x };
                fy = unsafe { next_e.motion.y };
                frx += unsafe { next_e.motion.xrel };
                fry += unsafe { next_e.motion.yrel };
            } else {
                state.pending_event = Some(next_e);
                break;
            }
        }
        return PollResult::Event(vec![
            Str("mousemoved"),
            Float(fx as f64 * scale_x as f64),
            Float(fy as f64 * scale_y as f64),
            Float(frx as f64 * scale_x as f64),
            Float(fry as f64 * scale_y as f64),
        ]);
    }

    if t == SDL_EVENT_MOUSE_WHEEL {
        // Match C backend: vertical first (positive = up), horizontal negated (positive = left).
        let (wx, wy, dir) = unsafe { (event.wheel.x, event.wheel.y, event.wheel.direction) };
        let (vy, vx) = if dir == SDL_MouseWheelDirection::FLIPPED {
            (-wy, -wx)
        } else {
            (wy, wx)
        };
        return PollResult::Event(vec![Str("mousewheel"), Float(vy as f64), Float(-vx as f64)]);
    }

    if t == SDL_EVENT_DROP_FILE {
        let data_ptr = unsafe { event.drop.data };
        if data_ptr.is_null() {
            return PollResult::Skip;
        }
        let filename = unsafe { CStr::from_ptr(data_ptr) }
            .to_string_lossy()
            .into_owned();
        return PollResult::Event(vec![Str("filedropped"), String(filename)]);
    }

    PollResult::Skip
}

/// Translate a keycode+scancode+modifier into a key name string.
/// Returns None for keys that should be ignored (e.g., Kp5 with NumLock off).
///
/// Algorithm (mirrors the C backend):
/// 1. Numpad scancode + NumLock off → positional name from NUMPAD table.
/// 2. kc < 128 or kc has SDLK_SCANCODE_MASK → SDL_GetKeyName (lowercased).
/// 3. Non-Latin kc → SDL_GetScancodeName (lowercased).
fn key_name(keycode: SDL_Keycode, scancode: SDL_Scancode, keymod: SDL_Keymod) -> Option<String> {
    // Step 1: numpad when NumLock is off.
    if keymod.0 & SDL_KMOD_NUM.0 == 0 {
        let sc_val = scancode.0;
        let kp1 = SDL_SCANCODE_KP_1.0;
        if (kp1..=kp1 + 10).contains(&sc_val) {
            let nm = NUMPAD[(sc_val - kp1) as usize];
            return if nm.is_empty() {
                None
            } else {
                Some(nm.to_string())
            };
        }
    }

    // Step 2: ASCII or scancode-masked key → SDL_GetKeyName.
    let kc_val = keycode.0;
    if kc_val < 128 || (kc_val & SDLK_SCANCODE_MASK.0 != 0) {
        let name = unsafe {
            let ptr = SDL_GetKeyName(keycode);
            if ptr.is_null() {
                return None;
            }
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        };
        let lower = name.to_lowercase();
        return if lower.is_empty() { None } else { Some(lower) };
    }

    // Step 3: non-Latin — use scancode name so layout-independent shortcuts work.
    let name = unsafe {
        let ptr = SDL_GetScancodeName(scancode);
        if ptr.is_null() {
            return None;
        }
        CStr::from_ptr(ptr).to_string_lossy().into_owned()
    };
    if !name.is_empty() {
        return Some(name.to_lowercase());
    }

    None
}

fn button_name(btn: u8) -> Option<&'static str> {
    match btn {
        1 => Some("left"),
        2 => Some("middle"),
        3 => Some("right"),
        4 => Some("x1"),
        5 => Some("x2"),
        _ => None,
    }
}
