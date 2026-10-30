//! The window's frame under the Cocoa driver: IOSurfaces shown in turn as the
//! contents of a layer over SDL's content view. The window server composites
//! the surface the editor painted, so a frame lives in the buffer the editor
//! writes and the one the display reads, with no texture or drawable copies
//! between them. `SDL_GetWindowSurface` is never called on this path; that
//! call is what builds SDL's Metal renderer and window texture.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::CStr;
use std::sync::OnceLock;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::NSWindow;
use objc2_core_foundation::{
    CFDictionary, CFNumber, CFRetained, CFString, CFType, CGFloat, CGPoint, CGRect, CGSize,
};
use objc2_io_surface::{
    IOSurfaceLockOptions, IOSurfaceRef, kIOSurfaceBytesPerElement, kIOSurfaceBytesPerRow,
    kIOSurfaceHeight, kIOSurfacePixelFormat, kIOSurfaceWidth,
};
use objc2_quartz_core::{CALayer, CATransaction, kCAGravityTopLeft};
use sdl3_sys::everything::*;

use super::cache::RenRect;

/// Environment variable that keeps SDL's own window surface on macOS.
const SDL_SURFACE_ENV: &str = "LITE_ANVIL_SDL_SURFACE";

// IOSurface's four-character code for 32-bit pixels stored B, G, R, A, the
// memory order of SDL_PIXELFORMAT_ARGB8888 on little-endian Macs.
const BGRA: i32 = i32::from_be_bytes(*b"BGRA");

// The most buffers the window keeps: the one shown, one the window server
// may still be reading for the frame before, and one to paint. A third is
// made only while the other two are both in use.
const POOL: usize = 3;

/// A failure to show a frame through the window's layer.
#[derive(Debug, thiserror::Error)]
pub enum LayerError {
    #[error("the window has no AppKit content view")]
    NoContentView,
    #[error("the content view has no layer")]
    NoHostLayer,
    #[error("creating a {0}x{1} IOSurface failed")]
    Create(i32, i32),
    #[error("locking an IOSurface failed ({0})")]
    Lock(i32),
    #[error("the layer has no buffer to paint")]
    NoBuffer,
    #[error("wrapping an IOSurface in an SDL surface failed: {0}")]
    Wrap(String),
}

/// A copy of the pixels the layer shows, B, G, R, A, `width * 4` bytes a row.
#[derive(Debug, Clone, PartialEq)]
pub struct Captured {
    pub pixels: Vec<u8>,
    pub width: i32,
    pub height: i32,
}

struct Buffer {
    surface: CFRetained<IOSurfaceRef>,
    // The frame the buffer holds; a new buffer holds none.
    frame: Option<u64>,
}

struct WindowLayer {
    window: *mut SDL_Window,
    layer: Retained<CALayer>,
    width: i32,
    height: i32,
    buffers: Vec<Buffer>,
    // The buffer the layer shows.
    front: Option<usize>,
    // The number of the shown frame.
    frame: u64,
    // What each of the latest frames changed, by frame number, so a buffer
    // holding an earlier frame catches up by copying only those rectangles.
    history: VecDeque<(u64, Vec<RenRect>)>,
}

thread_local! {
    static LAYER: RefCell<Option<WindowLayer>> = const { RefCell::new(None) };
    static FAILED: Cell<bool> = const { Cell::new(false) };
}

/// Whether frames reach the window through the layer rather than SDL's
/// window surface: the Cocoa driver, unless `LITE_ANVIL_SDL_SURFACE` is set to
/// anything but `0` or a frame could not be shown this way.
pub(crate) fn applies() -> bool {
    static SDL_SURFACE: OnceLock<bool> = OnceLock::new();
    let sdl_surface =
        *SDL_SURFACE.get_or_init(|| std::env::var_os(SDL_SURFACE_ENV).is_some_and(|v| v != "0"));
    if FAILED.get() || sdl_surface {
        return false;
    }
    // SAFETY: SDL answers null before video init or a static driver name.
    let driver = unsafe { SDL_GetCurrentVideoDriver() };
    // SAFETY: a non-null driver name is a NUL-terminated static string.
    !driver.is_null() && unsafe { CStr::from_ptr(driver) } == c"cocoa"
}

/// Switches the window to SDL's surface for the rest of the session after
/// the layer failed, taking the layer away so SDL's view shows again.
pub(crate) fn abandon(err: &LayerError) {
    log::error!("macOS layer presentation failed, using SDL's window surface: {err}");
    FAILED.set(true);
    release();
}

/// Paints a frame of `width` x `height` pixels into a buffer the window
/// server is not reading, then shows it. `paint` receives an SDL surface over
/// that buffer and the rectangles to draw: `dirty`, or the whole frame when
/// the buffer cannot be brought up to the shown frame by copying.
pub(crate) fn present(
    window: *mut SDL_Window,
    width: i32,
    height: i32,
    dirty: &[RenRect],
    paint: impl FnOnce(*mut SDL_Surface, &[RenRect]),
) -> Result<(), LayerError> {
    if window.is_null() || width <= 0 || height <= 0 {
        return Ok(());
    }
    LAYER.with_borrow_mut(|slot| {
        let current = match slot.take() {
            Some(l) if l.window == window && l.width == width && l.height == height => l,
            // A resized window keeps its layer and takes buffers of the new size.
            Some(old) if old.window == window => WindowLayer::new(window, old.layer, width, height),
            Some(old) => {
                old.layer.removeFromSuperlayer();
                WindowLayer::new(window, attach(window)?, width, height)
            }
            None => WindowLayer::new(window, attach(window)?, width, height),
        };
        slot.insert(current).present(dirty, paint)
    })
}

/// Frees every buffer but the one shown.
pub(crate) fn trim() {
    LAYER.with_borrow_mut(|slot| {
        if let Some(layer) = slot.as_mut() {
            layer.trim();
        }
    });
}

/// Frees every buffer and empties the layer while the window is hidden or
/// minimized; the next frame paints the whole window.
pub(crate) fn clear() {
    LAYER.with_borrow_mut(|slot| {
        if let Some(layer) = slot.as_mut() {
            layer.clear();
        }
    });
}

/// Takes the layer and its buffers away, before the window is destroyed.
pub(crate) fn release() {
    if let Some(layer) = LAYER.with_borrow_mut(Option::take) {
        layer.layer.removeFromSuperlayer();
    }
}

/// What the layer shows, or `None` before the first frame.
pub(crate) fn capture() -> Result<Option<Captured>, LayerError> {
    LAYER.with_borrow(|slot| {
        let Some(layer) = slot.as_ref() else {
            return Ok(None);
        };
        let Some(front) = layer.front else {
            return Ok(None);
        };
        let shown = Locked::new(
            &layer.buffers[front].surface,
            IOSurfaceLockOptions::ReadOnly,
        )?;
        let row_bytes = layer.width as usize * 4;
        let mut pixels = Vec::with_capacity(row_bytes * layer.height as usize);
        for row in 0..layer.height {
            pixels.extend_from_slice(shown.row(row, 0, layer.width));
        }
        Ok(Some(Captured {
            pixels,
            width: layer.width,
            height: layer.height,
        }))
    })
}

impl WindowLayer {
    fn new(window: *mut SDL_Window, layer: Retained<CALayer>, width: i32, height: i32) -> Self {
        Self {
            window,
            layer,
            width,
            height,
            buffers: Vec::new(),
            front: None,
            frame: 0,
            history: VecDeque::new(),
        }
    }

    fn whole(&self) -> RenRect {
        RenRect {
            x: 0,
            y: 0,
            w: self.width,
            h: self.height,
        }
    }

    // A buffer to paint: one the window server is not reading, a new one
    // while the pool has room, else the one holding the oldest frame.
    fn target(&mut self) -> Result<usize, LayerError> {
        let front = self.front;
        let candidates = (0..self.buffers.len()).filter(|&i| Some(i) != front);
        if let Some(i) = candidates
            .clone()
            .find(|&i| !self.buffers[i].surface.is_in_use())
        {
            return Ok(i);
        }
        if self.buffers.len() < POOL {
            self.buffers.push(buffer(self.width, self.height)?);
            return Ok(self.buffers.len() - 1);
        }
        candidates
            .min_by_key(|&i| self.buffers[i].frame)
            .ok_or(LayerError::NoBuffer)
    }

    // What buffer `target` lacks of the shown frame: the rectangles every
    // later frame changed, or all of it when the history no longer reaches
    // back to the frame it holds.
    fn missing(&self, target: usize) -> Vec<RenRect> {
        let Some(held) = self.buffers[target].frame else {
            return vec![self.whole()];
        };
        if self
            .history
            .front()
            .is_none_or(|(first, _)| *first > held + 1)
        {
            return vec![self.whole()];
        }
        self.history
            .iter()
            .filter(|(frame, _)| *frame > held)
            .flat_map(|(_, rects)| rects.iter().copied())
            .collect()
    }

    fn present(
        &mut self,
        dirty: &[RenRect],
        paint: impl FnOnce(*mut SDL_Surface, &[RenRect]),
    ) -> Result<(), LayerError> {
        let whole = self.whole();
        let damage: Vec<RenRect> = if self.front.is_none() {
            vec![whole]
        } else {
            dirty
                .iter()
                .map(|r| r.intersect(whole))
                .filter(|r| !r.is_empty())
                .collect()
        };
        let back = self.target()?;
        {
            let mut target =
                Locked::new(&self.buffers[back].surface, IOSurfaceLockOptions::empty())?;
            let repaints_all = damage.contains(&whole);
            if let (Some(front), false) = (self.front, repaints_all) {
                let source =
                    Locked::new(&self.buffers[front].surface, IOSurfaceLockOptions::ReadOnly)?;
                for r in self.missing(back) {
                    for row in r.y..r.y + r.h {
                        target
                            .row_mut(row, r.x, r.w)
                            .copy_from_slice(source.row(row, r.x, r.w));
                    }
                }
            }
            // SAFETY: the locked buffer holds `height` rows of `stride` bytes
            // and stays locked until the wrapper is destroyed below.
            let wrapper = unsafe {
                SDL_CreateSurfaceFrom(
                    self.width,
                    self.height,
                    SDL_PIXELFORMAT_ARGB8888,
                    target.base.cast(),
                    target.stride as i32,
                )
            };
            if wrapper.is_null() {
                return Err(LayerError::Wrap(sdl_error()));
            }
            paint(wrapper, &damage);
            // SAFETY: the wrapper was created above and does not own its pixels.
            unsafe { SDL_DestroySurface(wrapper) };
        }
        self.frame += 1;
        self.buffers[back].frame = Some(self.frame);
        self.history.push_back((self.frame, damage));
        if self.history.len() > POOL {
            self.history.pop_front();
        }
        self.show(back);
        self.front = Some(back);
        Ok(())
    }

    // Makes buffer `index` the layer's contents in one transaction without
    // the implicit fade a contents change otherwise animates.
    fn show(&self, index: usize) {
        let (points_w, points_h) = point_size(self.window);
        let (pixels_w, _) = pixel_size(self.window);
        let scale = if points_w > 0 && pixels_w > 0 {
            CGFloat::from(pixels_w) / CGFloat::from(points_w)
        } else {
            1.0
        };
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        self.layer.setFrame(CGRect::new(
            CGPoint::new(0.0, 0.0),
            CGSize::new(CGFloat::from(points_w), CGFloat::from(points_h)),
        ));
        self.layer.setContentsScale(scale);
        let surface = &self.buffers[index].surface;
        // SAFETY: IOSurfaceRef is toll-free bridged to the IOSurface object a
        // layer takes as contents; the layer retains it.
        unsafe {
            let contents = CFRetained::as_ptr(surface).as_ptr().cast::<AnyObject>();
            self.layer.setContents(Some(&*contents));
        }
        CATransaction::commit();
    }

    fn trim(&mut self) {
        let Some(front) = self.front else {
            self.buffers.clear();
            return;
        };
        let shown = self.buffers.swap_remove(front);
        self.buffers = vec![shown];
        self.front = Some(0);
    }

    fn clear(&mut self) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        // SAFETY: clearing the contents takes no object.
        unsafe { self.layer.setContents(None) };
        CATransaction::commit();
        self.buffers.clear();
        self.front = None;
        self.history.clear();
    }
}

// The window's size in points, the units of its views and layers.
fn point_size(window: *mut SDL_Window) -> (i32, i32) {
    let (mut w, mut h) = (0, 0);
    // SAFETY: the window is live for the session, on the main thread.
    if !unsafe { SDL_GetWindowSize(window, &mut w, &mut h) } {
        return (0, 0);
    }
    (w, h)
}

fn pixel_size(window: *mut SDL_Window) -> (i32, i32) {
    let (mut w, mut h) = (0, 0);
    // SAFETY: the window is live for the session, on the main thread.
    if !unsafe { SDL_GetWindowSizeInPixels(window, &mut w, &mut h) } {
        return (0, 0);
    }
    (w, h)
}

// Adds an opaque layer over the window's content view and answers it.
fn attach(window: *mut SDL_Window) -> Result<Retained<CALayer>, LayerError> {
    // SAFETY: the window is live; SDL answers null when the property is unset.
    let ns_window = unsafe {
        SDL_GetPointerProperty(
            SDL_GetWindowProperties(window),
            SDL_PROP_WINDOW_COCOA_WINDOW_POINTER,
            std::ptr::null_mut(),
        )
    };
    if ns_window.is_null() {
        return Err(LayerError::NoContentView);
    }
    // SAFETY: SDL's NSWindow lives as long as its SDL window, which outlives
    // this call on the main thread.
    let ns_window = unsafe { &*ns_window.cast::<NSWindow>() };
    let view = ns_window.contentView().ok_or(LayerError::NoContentView)?;
    view.setWantsLayer(true);
    let host = view.layer().ok_or(LayerError::NoHostLayer)?;
    let layer = CALayer::new();
    // SAFETY: kCAGravityTopLeft is a constant Core Animation defines.
    layer.setContentsGravity(unsafe { kCAGravityTopLeft });
    layer.setOpaque(true);
    host.addSublayer(&layer);
    Ok(layer)
}

// A zeroed BGRA IOSurface of `width` x `height` pixels.
fn buffer(width: i32, height: i32) -> Result<Buffer, LayerError> {
    // SAFETY: the key is a constant IOSurface defines.
    let row = IOSurfaceRef::align_property(unsafe { kIOSurfaceBytesPerRow }, width as usize * 4);
    // SAFETY: the keys are constants IOSurface defines.
    let keys: [&CFString; 5] = unsafe {
        [
            kIOSurfaceWidth,
            kIOSurfaceHeight,
            kIOSurfaceBytesPerElement,
            kIOSurfaceBytesPerRow,
            kIOSurfacePixelFormat,
        ]
    };
    let values = [
        CFNumber::new_i32(width),
        CFNumber::new_i32(height),
        CFNumber::new_i32(4),
        CFNumber::new_i64(row as i64),
        CFNumber::new_i32(BGRA),
    ];
    let keys: Vec<&CFType> = keys.iter().map(|k| k.as_ref()).collect();
    let values: Vec<&CFType> = values.iter().map(|v| v.as_ref()).collect();
    let properties = CFDictionary::<CFType, CFType>::from_slices(&keys, &values);
    // SAFETY: every key names an IOSurface property and every value is the
    // CFNumber it expects.
    let surface = unsafe { IOSurfaceRef::new(properties.as_opaque()) }
        .ok_or(LayerError::Create(width, height))?;
    Ok(Buffer {
        surface,
        frame: None,
    })
}

fn sdl_error() -> String {
    // SAFETY: SDL_GetError answers a NUL-terminated string, never null.
    unsafe { CStr::from_ptr(SDL_GetError()) }
        .to_string_lossy()
        .into_owned()
}

// An IOSurface locked for the CPU until dropped.
struct Locked<'a> {
    surface: &'a IOSurfaceRef,
    options: IOSurfaceLockOptions,
    base: *mut u8,
    stride: usize,
}

impl<'a> Locked<'a> {
    fn new(surface: &'a IOSurfaceRef, options: IOSurfaceLockOptions) -> Result<Self, LayerError> {
        // SAFETY: the seed pointer may be null.
        let status = unsafe { surface.lock(options, std::ptr::null_mut()) };
        if status != 0 {
            return Err(LayerError::Lock(status));
        }
        Ok(Self {
            surface,
            options,
            base: surface.base_address().as_ptr().cast::<u8>(),
            stride: surface.bytes_per_row(),
        })
    }

    fn row(&self, row: i32, x: i32, width: i32) -> &[u8] {
        let at = row as usize * self.stride + x as usize * 4;
        // SAFETY: callers keep `row` below the surface height and `x + width`
        // within its width, so the span lies inside the locked allocation.
        unsafe { std::slice::from_raw_parts(self.base.add(at), width as usize * 4) }
    }

    fn row_mut(&mut self, row: i32, x: i32, width: i32) -> &mut [u8] {
        let at = row as usize * self.stride + x as usize * 4;
        // SAFETY: as in `row`; the lock is a write lock.
        unsafe { std::slice::from_raw_parts_mut(self.base.add(at), width as usize * 4) }
    }
}

impl Drop for Locked<'_> {
    fn drop(&mut self) {
        // SAFETY: the surface was locked with these options by `new`.
        unsafe { self.surface.unlock(self.options, std::ptr::null_mut()) };
    }
}
