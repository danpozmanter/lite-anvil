use freetype_sys::{
    FT_Done_Face, FT_FACE_FLAG_SCALABLE, FT_Get_Char_Index, FT_GlyphSlot, FT_Init_FreeType,
    FT_Int32, FT_LOAD_FORCE_AUTOHINT, FT_LOAD_NO_HINTING, FT_LOAD_TARGET_LCD, FT_LOAD_TARGET_LIGHT,
    FT_LOAD_TARGET_MONO, FT_Library, FT_Load_Char, FT_Load_Glyph, FT_New_Face, FT_RENDER_MODE_LCD,
    FT_RENDER_MODE_LIGHT, FT_RENDER_MODE_MONO, FT_RENDER_MODE_NORMAL, FT_Render_Glyph,
    FT_Render_Mode, FT_Set_Pixel_Sizes, FT_UInt, FT_ULong,
};
use parking_lot::Mutex;
use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::CString;
use std::sync::Arc;
use std::thread::{self, ThreadId};

// ── FreeType library handle (main thread) ─────────────────────────────────────

thread_local! {
    // Stored as usize so Cell<usize> (which is Copy) can be used without Send issues.
    static FT_LIB_PTR: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Return the thread-local FT_Library, initializing it on first call.
pub(super) fn ft_lib() -> Result<FT_Library, String> {
    FT_LIB_PTR.with(|c| {
        let ptr = c.get();
        if ptr != 0 {
            return Ok(ptr as FT_Library);
        }
        let mut lib: FT_Library = std::ptr::null_mut();
        // SAFETY: single-threaded; called once per thread.
        let err = unsafe { FT_Init_FreeType(&mut lib) };
        if err != 0 {
            return Err(format!("FreeType2 init failed: error {err}"));
        }
        c.set(lib as usize);
        Ok(lib)
    })
}

// ── Load flag not exported by the freetype-sys crate ─────────────────────────

const FT_LOAD_BITMAP_METRICS_ONLY: i32 = 1 << 22;

// pixel_mode constants (FT_Pixel_Mode_ variants as u8 values)
const PIXEL_MODE_GRAY: u8 = 2;
const PIXEL_MODE_LCD: u8 = 5;
thread_local! {
    static GLYPH_CACHE_LIMIT: Cell<usize> = const { Cell::new(2048) };
    static SKIP_PREWARM: Cell<bool> = const { Cell::new(false) };
}

/// Set the maximum glyph cache entries per font.
pub fn set_glyph_cache_limit(limit: usize) {
    GLYPH_CACHE_LIMIT.with(|c| c.set(limit));
}

fn glyph_cache_limit() -> usize {
    GLYPH_CACHE_LIMIT.with(|c| c.get())
}

/// Skip pre-populating the ASCII glyph cache on font load.
pub fn set_skip_prewarm(skip: bool) {
    SKIP_PREWARM.with(|c| c.set(skip));
}

/// Current value of the skip-prewarm flag.
pub(crate) fn skip_prewarm() -> bool {
    SKIP_PREWARM.with(|c| c.get())
}

thread_local! {
    /// Weak references to every `FontInner` loaded on this thread so
    /// memory-pressure paths (occluded window, macOS memory-pressure
    /// signal) can walk them and drop cached glyph bitmaps.
    static FONT_REGISTRY: std::cell::RefCell<Vec<std::sync::Weak<parking_lot::Mutex<FontInner>>>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Record a live font so `clear_glyph_caches` can find it later.
pub(crate) fn register_font(f: &FontRef) {
    FONT_REGISTRY.with(|r| {
        let mut r = r.borrow_mut();
        r.retain(|w| w.upgrade().is_some());
        r.push(std::sync::Arc::downgrade(f));
    });
}

/// Clear the per-font glyph cache for every font on this thread. The
/// next draw will re-rasterise glyphs on demand.
pub fn clear_glyph_caches() {
    FONT_REGISTRY.with(|r| {
        let mut r = r.borrow_mut();
        r.retain(|w| {
            if let Some(arc) = w.upgrade() {
                arc.lock().glyphs.clear();
                true
            } else {
                false
            }
        });
    });
}

// ── Antialiasing / Hinting ────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Antialiasing {
    None,
    Grayscale,
    #[default]
    Subpixel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Hinting {
    None,
    #[default]
    Slight,
    Full,
}

// ── Glyph data ────────────────────────────────────────────────────────────────

/// Cached per-glyph data.
///
/// The bitmap lives behind an `Arc` so cloning a `GlyphInfo` for the draw
/// path is a refcount bump rather than a copy of the pixel buffer.
#[derive(Clone)]
pub struct GlyphInfo {
    pub xadvance: f32,
    pub bitmap: Option<Arc<GlyphBitmap>>,
    /// False when the font has no cmap entry for the codepoint and this
    /// glyph is the face's .notdef box. Font-group fallback keys off this.
    pub defined: bool,
}

/// A cached glyph together with the access tick used for LRU eviction.
struct CachedGlyph {
    info: GlyphInfo,
    last_used: u64,
}

/// Raw pixel data for a rendered glyph.
#[derive(Clone)]
pub struct GlyphBitmap {
    /// Pixel data: grayscale = 1 byte/pixel, subpixel = 3 bytes/pixel (R,G,B).
    pub data: Vec<u8>,
    /// Pixel width (not byte width).
    pub width: u32,
    pub rows: u32,
    /// Byte width of one row in `data` (= width for gray, width*3 for LCD).
    pub row_bytes: u32,
    pub left: i32,
    pub top: i32,
    pub subpixel: bool,
}

// ── FontInner ─────────────────────────────────────────────────────────────────

pub struct FontInner {
    face: FT_Library, // actually FT_Face — reuse the pointer-sized type alias
    owner_thread: ThreadId,
    pub path: String,
    pub size: f32,
    pub tab_size: i32,
    pub height: i32,
    pub baseline: i32,
    pub space_advance: f32,
    pub antialiasing: Antialiasing,
    pub hinting: Hinting,
    glyphs: HashMap<u32, CachedGlyph>,
    /// Monotonic access counter; each glyph records the tick of its last
    /// use so the least-recently-used entry can be evicted at capacity.
    tick: u64,
}

// FT_Face is a C raw pointer. We run single-threaded so this is safe.
// SAFETY: The FT_Library / FT_Face are only used on the main thread.
unsafe impl Send for FontInner {}
unsafe impl Sync for FontInner {}

impl Drop for FontInner {
    fn drop(&mut self) {
        if self.face.is_null() || !self.on_owner_thread() {
            return;
        }
        // SAFETY: face is valid until drop; called on the owning thread.
        unsafe { FT_Done_Face(self.face as *mut _) };
    }
}

impl FontInner {
    fn on_owner_thread(&self) -> bool {
        thread::current().id() == self.owner_thread
    }

    pub fn load(
        path: &str,
        size: f32,
        antialiasing: Antialiasing,
        hinting: Hinting,
    ) -> Result<Self, String> {
        let c_path = CString::new(path).map_err(|e| e.to_string())?;
        let mut face: *mut freetype_sys::FT_FaceRec = std::ptr::null_mut();
        // SAFETY: library is valid; path is a valid C string.
        let lib = ft_lib()?;
        let err = unsafe { FT_New_Face(lib, c_path.as_ptr(), 0, &mut face) };
        if err != 0 {
            return Err(format!("FT_New_Face failed ({path}): error {err}"));
        }
        let mut inner = FontInner {
            face: face as FT_Library,
            owner_thread: thread::current().id(),
            path: path.to_string(),
            size,
            tab_size: 2,
            height: 0,
            baseline: 0,
            space_advance: 0.0,
            antialiasing,
            hinting,
            glyphs: HashMap::new(),
            tick: 0,
        };
        inner.recompute_metrics()?;
        inner.prewarm_ascii();
        Ok(inner)
    }

    fn raw_face(&self) -> *mut freetype_sys::FT_FaceRec {
        self.face as *mut _
    }

    pub fn recompute_metrics(&mut self) -> Result<(), String> {
        if !self.on_owner_thread() {
            self.glyphs.clear();
            return Ok(());
        }
        let face = self.raw_face();
        let err = unsafe { FT_Set_Pixel_Sizes(face, 0, self.size as FT_UInt) };
        if err != 0 {
            return Err(format!("FT_Set_Pixel_Sizes failed: error {err}"));
        }
        // SAFETY: face and face->size are valid after FT_Set_Pixel_Sizes.
        unsafe {
            let fr = &*face;
            if (fr.face_flags as u64) & (FT_FACE_FLAG_SCALABLE as u64) != 0 {
                self.height = ((fr.height as f32 / fr.units_per_EM as f32) * self.size) as i32;
                self.baseline = ((fr.ascender as f32 / fr.units_per_EM as f32) * self.size) as i32;
            } else {
                let m = &(*fr.size).metrics;
                self.height = (m.height >> 6) as i32;
                self.baseline = (m.ascender >> 6) as i32;
            }
        }
        // Space advance — load without hinting for accurate measurement.
        // SAFETY: face is valid after FT_Set_Pixel_Sizes; glyph slot is valid after successful load.
        let flags = (FT_LOAD_BITMAP_METRICS_ONLY | FT_LOAD_NO_HINTING) as FT_Int32;
        if unsafe { FT_Load_Char(face, b' ' as FT_ULong, flags) } == 0 {
            self.space_advance = unsafe { (*(*face).glyph).advance.x as f32 / 64.0 };
        }
        self.glyphs.clear();
        Ok(())
    }

    fn load_render_flags(&self) -> (FT_Int32, FT_Render_Mode) {
        match (self.antialiasing, self.hinting) {
            (Antialiasing::None, _) => (FT_LOAD_TARGET_MONO, FT_RENDER_MODE_MONO),
            (Antialiasing::Grayscale, Hinting::None) => (FT_LOAD_NO_HINTING, FT_RENDER_MODE_NORMAL),
            (Antialiasing::Grayscale, _) => (
                FT_LOAD_TARGET_LIGHT | FT_LOAD_FORCE_AUTOHINT,
                FT_RENDER_MODE_LIGHT,
            ),
            (Antialiasing::Subpixel, _) => (
                FT_LOAD_TARGET_LCD | FT_LOAD_FORCE_AUTOHINT,
                FT_RENDER_MODE_LCD,
            ),
        }
    }

    /// Pre-populate the glyph cache with printable ASCII (32..=126).
    fn prewarm_ascii(&mut self) {
        if SKIP_PREWARM.with(|c| c.get()) {
            return;
        }
        for cp in 32..=126u32 {
            if !self.glyphs.contains_key(&cp) {
                self.tick = self.tick.wrapping_add(1);
                let last_used = self.tick;
                let info = self.load_glyph(cp);
                self.glyphs.insert(cp, CachedGlyph { info, last_used });
            }
        }
    }

    pub fn get_glyph(&mut self, codepoint: u32) -> &GlyphInfo {
        self.tick = self.tick.wrapping_add(1);
        let now = self.tick;
        if let Some(cached) = self.glyphs.get_mut(&codepoint) {
            cached.last_used = now;
        } else {
            if self.glyphs.len() >= glyph_cache_limit() {
                self.evict_lru();
            }
            let info = self.load_glyph(codepoint);
            self.glyphs.insert(
                codepoint,
                CachedGlyph {
                    info,
                    last_used: now,
                },
            );
        }
        // SAFETY: the branch above guarantees the key exists.
        &self.glyphs[&codepoint].info
    }

    /// Pixel advance of `codepoint`, rasterising and caching the glyph as
    /// needed but returning only the advance - the bitmap is never cloned.
    pub fn glyph_advance(&mut self, codepoint: u32) -> f32 {
        self.get_glyph(codepoint).xadvance
    }

    /// Drop the least-recently-used glyph, preferring non-ASCII so the
    /// printable-ASCII working set stays resident as a floor.
    fn evict_lru(&mut self) {
        // Single pass tracks two candidates: the lowest-tick non-pinned entry
        // (the preferred victim) and the lowest-tick entry overall (the
        // fallback when every entry is pinned ASCII).
        let mut best_unpinned: Option<(u32, u64)> = None;
        let mut best_any: Option<(u32, u64)> = None;
        for (&cp, c) in &self.glyphs {
            let tick = c.last_used;
            if best_any.is_none_or(|(_, t)| tick < t) {
                best_any = Some((cp, tick));
            }
            if !(32..=126).contains(&cp) && best_unpinned.is_none_or(|(_, t)| tick < t) {
                best_unpinned = Some((cp, tick));
            }
        }
        if let Some((cp, _)) = best_unpinned.or(best_any) {
            self.glyphs.remove(&cp);
        }
    }

    fn load_glyph(&self, codepoint: u32) -> GlyphInfo {
        if !self.on_owner_thread() {
            // Off-thread dummy: claim defined so group fallback isn't consulted.
            return GlyphInfo {
                xadvance: self.space_advance,
                bitmap: None,
                defined: true,
            };
        }
        let face = self.raw_face();
        // SAFETY: face is valid; glyph slot is valid after successful FT_Load_Glyph.
        let glyph_id: FT_UInt = unsafe { FT_Get_Char_Index(face, codepoint as FT_ULong) };
        let defined = glyph_id != 0;

        // Load without hinting to get the accurate xadvance.
        let no_hint = (FT_LOAD_BITMAP_METRICS_ONLY | FT_LOAD_NO_HINTING) as FT_Int32;
        let xadvance = if unsafe { FT_Load_Glyph(face, glyph_id, no_hint) } == 0 {
            unsafe { (*(*face).glyph).advance.x as f32 / 64.0 }
        } else {
            self.space_advance
        };

        if is_whitespace(codepoint) {
            return GlyphInfo {
                xadvance,
                bitmap: None,
                defined,
            };
        }

        let (load_flags, render_mode) = self.load_render_flags();
        // SAFETY: face is valid; load and render are called in sequence.
        let ok = unsafe {
            FT_Load_Glyph(face, glyph_id, load_flags) == 0
                && FT_Render_Glyph((*face).glyph, render_mode) == 0
        };
        if !ok {
            return GlyphInfo {
                xadvance,
                bitmap: None,
                defined,
            };
        }

        // SAFETY: glyph slot is valid after successful FT_Render_Glyph above.
        let bitmap = unsafe { copy_glyph_bitmap((*face).glyph) };
        GlyphInfo {
            xadvance,
            bitmap: bitmap.map(Arc::new),
            defined,
        }
    }
}

/// Copy the current glyph slot bitmap into an owned `GlyphBitmap`.
///
/// SAFETY: `slot` must point to a valid, rendered glyph slot.
unsafe fn copy_glyph_bitmap(slot: FT_GlyphSlot) -> Option<GlyphBitmap> {
    unsafe {
        let bm = &(*slot).bitmap;
        if bm.width == 0 || bm.rows == 0 || bm.buffer.is_null() {
            return None;
        }
        // FT_Pixel_Mode values are small and non-negative; comparing as u8
        // is correct whether the platform's c_char is signed or unsigned.
        let subpixel = bm.pixel_mode as u8 == PIXEL_MODE_LCD;
        let gray = bm.pixel_mode as u8 == PIXEL_MODE_GRAY;
        if !subpixel && !gray {
            return None; // unsupported mode
        }
        let width = bm.width as u32;
        let pixel_width = if subpixel { width / 3 } else { width };
        let row_bytes = width; // bytes per row (3*pixel_width for LCD)
        let total = bm.rows as usize * bm.pitch.unsigned_abs() as usize;
        let mut data = Vec::with_capacity(total);
        // FreeType defines `pitch` as the signed offset to add to step down one
        // row: positive for a down-flow bitmap, negative for an up-flow one
        // (where `buffer` points at the top row, which is the highest address).
        // Signed `.offset()` therefore walks rows top-to-bottom for either sign;
        // the old `(row * pitch) as usize` + `.add()` overflowed to a wild
        // pointer whenever pitch was negative.
        let pitch = bm.pitch as isize;
        for row in 0..bm.rows as isize {
            let row_ptr = bm.buffer.offset(row * pitch);
            data.extend_from_slice(std::slice::from_raw_parts(row_ptr, row_bytes as usize));
        }
        Some(GlyphBitmap {
            data,
            width: pixel_width,
            rows: bm.rows as u32,
            row_bytes,
            left: (*slot).bitmap_left,
            top: (*slot).bitmap_top,
            subpixel,
        })
    }
}

// ── Whitespace check ──────────────────────────────────────────────────────────

pub fn is_whitespace(cp: u32) -> bool {
    matches!(
        cp,
        0x20 | 0x85 | 0xA0 | 0x1680 | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000
    ) || (0x9..=0xD).contains(&cp)
        || (0x2000..=0x200A).contains(&cp)
}

// ── RenFont ──────────────────────────────────────────────────────────────────

pub type FontRef = Arc<Mutex<FontInner>>;

#[cfg(test)]
mod font_tests {
    use super::*;

    fn load_inner() -> FontInner {
        let path = format!(
            "{}/../data/fonts/Lilex-Regular.ttf",
            env!("CARGO_MANIFEST_DIR")
        );
        FontInner::load(&path, 15.0, Antialiasing::Grayscale, Hinting::Slight)
            .expect("bundled font loads")
    }

    #[test]
    fn glyph_advance_matches_full_glyph() {
        let mut font = load_inner();
        let full = font.get_glyph('M' as u32).xadvance;
        assert_eq!(font.glyph_advance('M' as u32), full);
    }

    #[test]
    fn lru_evicts_only_least_recently_used() {
        set_skip_prewarm(true);
        set_glyph_cache_limit(8);
        let mut font = load_inner();
        let hot = 0x4E00u32;
        font.get_glyph(hot);
        for cp in 0x4E01..0x4E40u32 {
            font.get_glyph(cp);
            // Keep `hot` the most-recently-used so LRU never evicts it.
            font.get_glyph(hot);
        }
        assert!(
            font.glyphs.contains_key(&hot),
            "recently-used glyph stays resident under LRU"
        );
        assert!(
            font.glyphs.len() <= 8,
            "cache respects the configured limit"
        );
        set_glyph_cache_limit(2048);
        set_skip_prewarm(false);
    }

    #[test]
    fn ascii_is_pinned_against_eviction() {
        set_glyph_cache_limit(128);
        let mut font = load_inner();
        for cp in 0x4E00..0x4F00u32 {
            font.get_glyph(cp);
        }
        assert!(
            font.glyphs.contains_key(&('A' as u32)),
            "printable ASCII stays pinned as the eviction floor"
        );
        set_glyph_cache_limit(2048);
    }
}
