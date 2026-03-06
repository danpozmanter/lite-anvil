use freetype::freetype::{
    FT_Done_Face, FT_FACE_FLAG_SCALABLE, FT_Get_Char_Index, FT_GlyphSlot, FT_Init_FreeType,
    FT_Int32, FT_LOAD_FORCE_AUTOHINT, FT_LOAD_NO_HINTING, FT_Library, FT_Load_Char, FT_Load_Glyph,
    FT_New_Face, FT_Render_Glyph, FT_Render_Mode, FT_Render_Mode_::*, FT_Set_Pixel_Sizes, FT_UInt,
    FT_ULong,
};
use mlua::prelude::*;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::ffi::CString;
use std::sync::Arc;

// ── FreeType library handle (main thread) ─────────────────────────────────────

thread_local! {
    // Stored as usize so Cell<usize> (which is Copy) can be used without Send issues.
    static FT_LIB_PTR: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Return the thread-local FT_Library, initializing it on first call.
pub(super) fn ft_lib() -> FT_Library {
    FT_LIB_PTR.with(|c| {
        let ptr = c.get();
        if ptr != 0 {
            return ptr as FT_Library;
        }
        let mut lib: FT_Library = std::ptr::null_mut();
        // SAFETY: single-threaded; called once per thread.
        let err = unsafe { FT_Init_FreeType(&mut lib) };
        assert!(err == 0, "FreeType2 init failed: error {err}");
        c.set(lib as usize);
        lib
    })
}

// ── Load flags not exported by the freetype 0.7.2 crate ──────────────────────

const FT_LOAD_BITMAP_METRICS_ONLY: i32 = 1 << 22;
const FT_LOAD_TARGET_LIGHT: i32 = 1 << 16;
const FT_LOAD_TARGET_MONO: i32 = 2 << 16;
const FT_LOAD_TARGET_LCD: i32 = 3 << 16;

// pixel_mode constants (FT_Pixel_Mode_ variants as u8 values)
const PIXEL_MODE_GRAY: u8 = 2;
const PIXEL_MODE_LCD: u8 = 5;

// ── Antialiasing / Hinting ────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum Antialiasing {
    None,
    Grayscale,
    #[default]
    Subpixel,
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum Hinting {
    None,
    #[default]
    Slight,
    Full,
}

// ── Glyph data ────────────────────────────────────────────────────────────────

/// Cached per-glyph data.
#[derive(Clone)]
pub struct GlyphInfo {
    pub xadvance: f32,
    pub bitmap: Option<GlyphBitmap>,
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
    pub path: String,
    pub size: f32,
    pub tab_size: i32,
    pub height: i32,
    pub baseline: i32,
    pub space_advance: f32,
    pub antialiasing: Antialiasing,
    pub hinting: Hinting,
    glyphs: HashMap<u32, GlyphInfo>,
}

// FT_Face is a C raw pointer. We run single-threaded so this is safe.
// SAFETY: The FT_Library / FT_Face are only used on the main Lua thread.
unsafe impl Send for FontInner {}
unsafe impl Sync for FontInner {}

impl Drop for FontInner {
    fn drop(&mut self) {
        // SAFETY: face is valid until drop; called on main thread.
        unsafe { FT_Done_Face(self.face as *mut _) };
    }
}

impl FontInner {
    pub fn load(
        path: &str,
        size: f32,
        antialiasing: Antialiasing,
        hinting: Hinting,
    ) -> Result<Self, String> {
        let c_path = CString::new(path).map_err(|e| e.to_string())?;
        let mut face: *mut freetype::freetype::FT_FaceRec_ = std::ptr::null_mut();
        // SAFETY: library is valid; path is a valid C string.
        let err = unsafe { FT_New_Face(ft_lib(), c_path.as_ptr(), 0, &mut face) };
        if err != 0 {
            return Err(format!("FT_New_Face failed ({path}): error {err}"));
        }
        let mut inner = FontInner {
            face: face as FT_Library,
            path: path.to_string(),
            size,
            tab_size: 2,
            height: 0,
            baseline: 0,
            space_advance: 0.0,
            antialiasing,
            hinting,
            glyphs: HashMap::new(),
        };
        inner.recompute_metrics()?;
        Ok(inner)
    }

    fn raw_face(&self) -> *mut freetype::freetype::FT_FaceRec_ {
        self.face as *mut _
    }

    pub fn recompute_metrics(&mut self) -> Result<(), String> {
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
        let flags = (FT_LOAD_BITMAP_METRICS_ONLY | FT_LOAD_NO_HINTING as i32) as FT_Int32;
        if unsafe { FT_Load_Char(face, b' ' as FT_ULong, flags) } == 0 {
            self.space_advance = unsafe { (*(*face).glyph).advance.x as f32 / 64.0 };
        }
        self.glyphs.clear();
        Ok(())
    }

    fn load_render_flags(&self) -> (FT_Int32, FT_Render_Mode) {
        match (self.antialiasing, self.hinting) {
            (Antialiasing::None, _) => (FT_LOAD_TARGET_MONO, FT_RENDER_MODE_MONO),
            (Antialiasing::Grayscale, Hinting::None) => {
                (FT_LOAD_NO_HINTING as i32, FT_RENDER_MODE_NORMAL)
            }
            (Antialiasing::Grayscale, _) => (
                FT_LOAD_TARGET_LIGHT | FT_LOAD_FORCE_AUTOHINT as i32,
                FT_RENDER_MODE_LIGHT,
            ),
            (Antialiasing::Subpixel, _) => (
                FT_LOAD_TARGET_LCD | FT_LOAD_FORCE_AUTOHINT as i32,
                FT_RENDER_MODE_LCD,
            ),
        }
    }

    pub fn get_glyph(&mut self, codepoint: u32) -> &GlyphInfo {
        if !self.glyphs.contains_key(&codepoint) {
            let info = self.load_glyph(codepoint);
            self.glyphs.insert(codepoint, info);
        }
        self.glyphs.get(&codepoint).unwrap()
    }

    fn load_glyph(&self, codepoint: u32) -> GlyphInfo {
        let face = self.raw_face();
        let glyph_id: FT_UInt = unsafe { FT_Get_Char_Index(face, codepoint as FT_ULong) };

        // Load without hinting to get the accurate xadvance.
        let no_hint = (FT_LOAD_BITMAP_METRICS_ONLY | FT_LOAD_NO_HINTING as i32) as FT_Int32;
        let xadvance = if unsafe { FT_Load_Glyph(face, glyph_id, no_hint) } == 0 {
            unsafe { (*(*face).glyph).advance.x as f32 / 64.0 }
        } else {
            self.space_advance
        };

        if is_whitespace(codepoint) {
            return GlyphInfo {
                xadvance,
                bitmap: None,
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
            };
        }

        let bitmap = unsafe { copy_glyph_bitmap((*face).glyph) };
        GlyphInfo { xadvance, bitmap }
    }

    /// Compute rendered width of `text` in pixels.
    /// `tab_offset` is the distance from the line's left edge to the start of `text`,
    /// used to align tab stops: adv = tab_w - (w + tab_offset) % tab_w.
    pub fn text_width(&mut self, text: &str, tab_offset: f32) -> f32 {
        let tab_w = self.space_advance * self.tab_size as f32;
        let mut w = 0.0f32;
        for ch in text.chars() {
            let cp = ch as u32;
            if cp == b'\t' as u32 {
                let r = (w + tab_offset).rem_euclid(tab_w);
                w += if r == 0.0 { tab_w } else { tab_w - r };
                continue;
            }
            let adv = if !is_whitespace(cp) {
                let g = self.get_glyph(cp).clone();
                if g.xadvance > 0.0 {
                    g.xadvance
                } else {
                    self.space_advance
                }
            } else {
                self.space_advance
            };
            w += adv;
        }
        w
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
        let subpixel = bm.pixel_mode == PIXEL_MODE_LCD;
        let gray = bm.pixel_mode == PIXEL_MODE_GRAY;
        if !subpixel && !gray {
            return None; // unsupported mode
        }
        let pixel_width = if subpixel { bm.width / 3 } else { bm.width };
        let row_bytes = bm.width; // bytes per row (3*pixel_width for LCD)
        let total = bm.rows as usize * bm.pitch.unsigned_abs() as usize;
        let mut data = Vec::with_capacity(total);
        for row in 0..bm.rows as isize {
            let offset = (row * bm.pitch as isize) as usize;
            data.extend_from_slice(std::slice::from_raw_parts(
                bm.buffer.add(offset),
                row_bytes as usize,
            ));
        }
        Some(GlyphBitmap {
            data,
            width: pixel_width,
            rows: bm.rows,
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

// ── RenFont (Lua UserData) ────────────────────────────────────────────────────

pub type FontRef = Arc<Mutex<FontInner>>;

/// A font or font-group handle exposed to Lua.
/// A single font has `fonts.len() == 1`; a group has multiple entries.
pub struct RenFont(pub Vec<FontRef>);

impl RenFont {
    pub fn new(inner: FontInner) -> Self {
        RenFont(vec![Arc::new(Mutex::new(inner))])
    }

    fn first(&self) -> parking_lot::MutexGuard<'_, FontInner> {
        self.0[0].lock()
    }

    pub fn get_width(&self, text: &str, tab_offset: f32) -> f32 {
        self.0[0].lock().text_width(text, tab_offset)
    }
    pub fn get_height(&self) -> i32 {
        self.first().height
    }
    pub fn get_size(&self) -> f32 {
        self.first().size
    }
    pub fn get_tab_size(&self) -> i32 {
        self.first().tab_size
    }
    pub fn set_size(&self, size: f32) {
        for f in &self.0 {
            let mut g = f.lock();
            g.size = size;
            let _ = g.recompute_metrics();
        }
    }
    pub fn set_tab_size(&self, n: i32) {
        for f in &self.0 {
            f.lock().tab_size = n;
        }
    }
    pub fn get_path(&self) -> String {
        self.first().path.clone()
    }

    pub fn copy_with(
        &self,
        size: Option<f32>,
        antialiasing: Option<Antialiasing>,
        hinting: Option<Hinting>,
    ) -> Result<RenFont, String> {
        let mut refs = Vec::with_capacity(self.0.len());
        for arc in &self.0 {
            let g = arc.lock();
            let aa = antialiasing.unwrap_or(g.antialiasing);
            let h = hinting.unwrap_or(g.hinting);
            let sz = size.unwrap_or(g.size);
            let new_inner = FontInner::load(&g.path, sz, aa, h)?;
            refs.push(Arc::new(Mutex::new(new_inner)));
        }
        Ok(RenFont(refs))
    }
}

// RenFont is registered as UserData only as an opaque inner handle;
// all Lua-visible methods live on the renderer.font table in mod.rs.
impl LuaUserData for RenFont {}

// ── Parse font option tables ──────────────────────────────────────────────────

pub fn parse_font_opts(t: &LuaTable) -> Result<(Option<Antialiasing>, Option<Hinting>), String> {
    let aa = t
        .get::<Option<String>>("antialiasing")
        .ok()
        .flatten()
        .map(|s| parse_antialiasing(&s))
        .transpose()?;
    let h = t
        .get::<Option<String>>("hinting")
        .ok()
        .flatten()
        .map(|s| parse_hinting(&s))
        .transpose()?;
    Ok((aa, h))
}

pub fn parse_antialiasing(s: &str) -> Result<Antialiasing, String> {
    match s {
        "none" => Ok(Antialiasing::None),
        "grayscale" => Ok(Antialiasing::Grayscale),
        "subpixel" => Ok(Antialiasing::Subpixel),
        other => Err(format!("unknown antialiasing: {other}")),
    }
}

pub fn parse_hinting(s: &str) -> Result<Hinting, String> {
    match s {
        "none" => Ok(Hinting::None),
        "slight" => Ok(Hinting::Slight),
        "full" => Ok(Hinting::Full),
        other => Err(format!("unknown hinting: {other}")),
    }
}
