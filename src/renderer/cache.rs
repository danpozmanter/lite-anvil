use super::font::{FontRef, GlyphInfo, is_whitespace};
use sdl3_sys::everything::*;

// ── Constants ─────────────────────────────────────────────────────────────────

pub const CELLS_X: usize = 80;
pub const CELLS_Y: usize = 50;
const CELL_SIZE: i32 = 96;
const HASH_INITIAL: u32 = 0x811C9DC5;
const FNV_PRIME: u32 = 0x01000193;

// ── RenColor / RenRect ────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Default)]
pub struct RenColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RenRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl RenRect {
    pub fn overlaps(self, o: RenRect) -> bool {
        o.x + o.w > self.x && o.x < self.x + self.w && o.y + o.h > self.y && o.y < self.y + self.h
    }

    pub fn intersect(self, o: RenRect) -> RenRect {
        let x1 = self.x.max(o.x);
        let y1 = self.y.max(o.y);
        let x2 = (self.x + self.w).min(o.x + o.w);
        let y2 = (self.y + self.h).min(o.y + o.h);
        RenRect {
            x: x1,
            y: y1,
            w: (x2 - x1).max(0),
            h: (y2 - y1).max(0),
        }
    }

    pub fn merge(self, o: RenRect) -> RenRect {
        let x1 = self.x.min(o.x);
        let y1 = self.y.min(o.y);
        let x2 = (self.x + self.w).max(o.x + o.w);
        let y2 = (self.y + self.h).max(o.y + o.h);
        RenRect {
            x: x1,
            y: y1,
            w: x2 - x1,
            h: y2 - y1,
        }
    }

    pub fn is_empty(self) -> bool {
        self.w <= 0 || self.h <= 0
    }
}

// ── Commands ──────────────────────────────────────────────────────────────────

pub struct DrawTextCmd {
    pub fonts: Vec<FontRef>,
    pub text: String,
    pub x: f32,
    pub y: i32,
    pub color: RenColor,
    /// Distance from the line's left edge to x; used for tab-stop alignment.
    pub tab_offset: f32,
    /// Bounding rect used for overlap check (pre-computed).
    pub bounding: RenRect,
}

pub enum Command {
    SetClip(RenRect),
    DrawRect { rect: RenRect, color: RenColor },
    DrawText(DrawTextCmd),
}

// ── RenCache ──────────────────────────────────────────────────────────────────

pub struct RenCache {
    pub commands: Vec<Command>,
    cells: [u32; CELLS_X * CELLS_Y],
    cells_prev: [u32; CELLS_X * CELLS_Y],
    pub screen: RenRect,
    pub last_clip: RenRect,
    pub show_debug: bool,
}

impl RenCache {
    pub fn new() -> Self {
        let mut c = RenCache {
            commands: Vec::new(),
            cells: [HASH_INITIAL; CELLS_X * CELLS_Y],
            cells_prev: [0xFF_FF_FF_FF; CELLS_X * CELLS_Y],
            screen: RenRect::default(),
            last_clip: RenRect::default(),
            show_debug: false,
        };
        // cells_prev = 0xFFFFFFFF → first frame fully dirty.
        c.cells_prev.fill(0xFF_FF_FF_FF);
        c
    }

    pub fn invalidate(&mut self) {
        self.cells_prev.fill(0xFF_FF_FF_FF);
    }

    pub fn begin_frame(&mut self, w: i32, h: i32) {
        if self.screen.w != w || self.screen.h != h {
            self.screen = RenRect { x: 0, y: 0, w, h };
            self.invalidate();
        }
        self.last_clip = self.screen;
        self.commands.clear();
        // Reset cells to HASH_INITIAL for this frame's hash accumulation.
        self.cells.fill(HASH_INITIAL);
    }

    pub fn push_set_clip(&mut self, rect: RenRect) {
        let r = rect.intersect(self.screen);
        self.last_clip = r;
        self.commands.push(Command::SetClip(r));
    }

    pub fn push_draw_rect(&mut self, rect: RenRect, color: RenColor) {
        if rect.w == 0 || rect.h == 0 || !self.last_clip.overlaps(rect) {
            return;
        }
        self.commands.push(Command::DrawRect { rect, color });
    }

    /// Push a DrawText command. Returns the new x position after the text.
    pub fn push_draw_text(
        &mut self,
        fonts: Vec<FontRef>,
        text: String,
        x: f32,
        y: i32,
        color: RenColor,
        tab_offset: f32,
    ) -> f32 {
        let width = fonts[0].lock().text_width(&text, tab_offset);
        let height = fonts[0].lock().height;
        let bounding = RenRect {
            x: x as i32,
            y,
            w: width as i32,
            h: height,
        };
        if self.last_clip.overlaps(bounding) {
            self.commands.push(Command::DrawText(DrawTextCmd {
                fonts,
                text,
                x,
                y,
                color,
                tab_offset,
                bounding,
            }));
        }
        x + width
    }

    /// Hash all commands into the cell grid, then find dirty rects.
    pub fn compute_dirty_rects(&mut self) -> Vec<RenRect> {
        // Accumulate hashes for each command into the overlapping cells.
        for cmd in &self.commands {
            let (rect, h) = cmd_hash(cmd);
            if rect.is_empty() {
                continue;
            }
            let clipped = rect.intersect(self.screen);
            if clipped.is_empty() {
                continue;
            }
            update_cells(&mut self.cells, clipped, h);
        }

        // Find changed cells → dirty rects.
        let mut dirty: Vec<RenRect> = Vec::new();
        let max_x = (self.screen.w / CELL_SIZE + 1) as usize;
        let max_y = (self.screen.h / CELL_SIZE + 1) as usize;
        for cy in 0..max_y.min(CELLS_Y) {
            for cx in 0..max_x.min(CELLS_X) {
                let idx = cx + cy * CELLS_X;
                if self.cells[idx] != self.cells_prev[idx] {
                    let r = RenRect {
                        x: cx as i32 * CELL_SIZE,
                        y: cy as i32 * CELL_SIZE,
                        w: CELL_SIZE,
                        h: CELL_SIZE,
                    };
                    push_rect(&mut dirty, r.intersect(self.screen));
                }
            }
        }

        // Save current cells as previous for next frame comparison, reset current.
        self.cells_prev.copy_from_slice(&self.cells);
        self.cells.fill(HASH_INITIAL);

        dirty
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn fnv1a_update(h: &mut u32, data: &[u8]) {
    for &b in data {
        *h ^= b as u32;
        *h = h.wrapping_mul(FNV_PRIME);
    }
}

/// Compute the (rect, hash) pair for a command — no heap allocation.
fn cmd_hash(cmd: &Command) -> (RenRect, u32) {
    let mut h = HASH_INITIAL;
    match cmd {
        Command::SetClip(r) => {
            let bytes = bytepack_i32x4(r.x, r.y, r.w, r.h);
            fnv1a_update(&mut h, &bytes);
            (*r, h)
        }
        Command::DrawRect { rect: r, color: c } => {
            let rect_bytes = bytepack_i32x4(r.x, r.y, r.w, r.h);
            fnv1a_update(&mut h, &rect_bytes);
            fnv1a_update(&mut h, &[c.r, c.g, c.b, c.a]);
            (*r, h)
        }
        Command::DrawText(dt) => {
            let r = dt.bounding;
            fnv1a_update(&mut h, dt.text.as_bytes());
            fnv1a_update(&mut h, &dt.x.to_bits().to_ne_bytes());
            fnv1a_update(&mut h, &dt.y.to_ne_bytes());
            fnv1a_update(&mut h, &[dt.color.r, dt.color.g, dt.color.b, dt.color.a]);
            (r, h)
        }
    }
}

#[inline(always)]
fn bytepack_i32x4(a: i32, b: i32, c: i32, d: i32) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a.to_ne_bytes());
    out[4..8].copy_from_slice(&b.to_ne_bytes());
    out[8..12].copy_from_slice(&c.to_ne_bytes());
    out[12..16].copy_from_slice(&d.to_ne_bytes());
    out
}

fn update_cells(cells: &mut [u32; CELLS_X * CELLS_Y], r: RenRect, h: u32) {
    let x1 = (r.x / CELL_SIZE) as usize;
    let y1 = (r.y / CELL_SIZE) as usize;
    let x2 = ((r.x + r.w) / CELL_SIZE) as usize;
    let y2 = ((r.y + r.h) / CELL_SIZE) as usize;
    let h_bytes = h.to_ne_bytes();
    for cy in y1..=y2.min(CELLS_Y - 1) {
        for cx in x1..=x2.min(CELLS_X - 1) {
            let idx = cx + cy * CELLS_X;
            fnv1a_update(&mut cells[idx], &h_bytes);
        }
    }
}

/// Merge `r` into an existing touching-or-overlapping rect in `dirty`, or push a new one.
/// Uses inclusive bounds (>=) so adjacent cells sharing an edge are merged, matching the
/// behaviour of the original C rencache and preventing O(cells × commands) rendering cost.
fn push_rect(dirty: &mut Vec<RenRect>, r: RenRect) {
    if r.is_empty() {
        return;
    }
    for existing in dirty.iter_mut().rev() {
        let e = *existing;
        if r.x + r.w >= e.x && r.x <= e.x + e.w && r.y + r.h >= e.y && r.y <= e.y + e.h {
            *existing = existing.merge(r);
            return;
        }
    }
    dirty.push(r);
}

// ── Surface drawing ───────────────────────────────────────────────────────────

/// Pixel-level RGBA components unpacked from a 32-bit surface pixel.
struct PixFmt {
    rshift: u8,
    gshift: u8,
    bshift: u8,
    ashift: u8,
}

impl PixFmt {
    unsafe fn from_sdl(details: *const SDL_PixelFormatDetails) -> Self {
        unsafe {
            PixFmt {
                rshift: (*details).Rshift,
                gshift: (*details).Gshift,
                bshift: (*details).Bshift,
                ashift: (*details).Ashift,
            }
        }
    }

    fn pack(&self, r: u8, g: u8, b: u8, a: u8) -> u32 {
        (r as u32) << self.rshift
            | (g as u32) << self.gshift
            | (b as u32) << self.bshift
            | (a as u32) << self.ashift
    }

    fn unpack(&self, px: u32) -> (u8, u8, u8, u8) {
        (
            ((px >> self.rshift) & 0xFF) as u8,
            ((px >> self.gshift) & 0xFF) as u8,
            ((px >> self.bshift) & 0xFF) as u8,
            ((px >> self.ashift) & 0xFF) as u8,
        )
    }
}

/// Draw all commands onto the SDL3 window surface within each dirty rect.
///
/// SAFETY: The surface pointer must be valid. Called on the main thread.
pub unsafe fn render_dirty_rects(
    surface: *mut SDL_Surface,
    commands: &[Command],
    dirty: &[RenRect],
) {
    if dirty.is_empty() {
        return;
    }

    let (fmt, pitch, pixels, surface_bounds) = unsafe {
        let details = SDL_GetPixelFormatDetails((*surface).format);
        let fmt = PixFmt::from_sdl(details);
        let pitch = (*surface).pitch as usize;
        let pixels = (*surface).pixels as *mut u8;
        if pixels.is_null() {
            return;
        }
        // SDL_GetWindowSizeInPixels (used for screen.h in begin_frame) can
        // differ from the actual surface dimensions when the window manager
        // hasn't yet applied a resize request.  Clamp all pixel access to the
        // real surface bounds so we never walk off the end of the buffer.
        let bounds = RenRect {
            x: 0,
            y: 0,
            w: (*surface).w,
            h: (*surface).h,
        };
        (fmt, pitch, pixels, bounds)
    };

    for &dirty_rect in dirty {
        let dirty_rect = dirty_rect.intersect(surface_bounds);
        if dirty_rect.is_empty() {
            continue;
        }
        let sdl_clip = SDL_Rect {
            x: dirty_rect.x,
            y: dirty_rect.y,
            w: dirty_rect.w,
            h: dirty_rect.h,
        };
        unsafe { SDL_SetSurfaceClipRect(surface, &sdl_clip) };

        let mut clip = dirty_rect;

        for cmd in commands {
            match cmd {
                Command::SetClip(r) => {
                    clip = r.intersect(dirty_rect);
                }
                Command::DrawRect { rect, color } => unsafe {
                    draw_rect_surface(surface, pixels, pitch, &fmt, *rect, *color, clip);
                },
                Command::DrawText(dt) => {
                    unsafe { draw_text_surface(pixels, pitch, &fmt, dt, clip) };
                }
            }
        }
    }

    unsafe { SDL_SetSurfaceClipRect(surface, std::ptr::null()) };
}

/// Fill a rectangle with a solid color, respecting the clip rect.
unsafe fn draw_rect_surface(
    surface: *mut SDL_Surface,
    pixels: *mut u8,
    pitch: usize,
    fmt: &PixFmt,
    rect: RenRect,
    color: RenColor,
    clip: RenRect,
) {
    if color.a == 0 {
        return;
    }
    let r = rect.intersect(clip);
    if r.is_empty() {
        return;
    }

    if color.a == 255 {
        // Fast opaque fill via SDL.
        let sdl_rect = SDL_Rect {
            x: r.x,
            y: r.y,
            w: r.w,
            h: r.h,
        };
        unsafe {
            let pixel = SDL_MapSurfaceRGB(surface, color.r, color.g, color.b);
            SDL_FillSurfaceRect(surface, &sdl_rect, pixel);
        }
    } else {
        // Alpha-blended fill: blend each destination pixel.
        let ia = 255 - color.a as u32;
        for row in r.y..r.y + r.h {
            unsafe {
                let row_ptr = pixels.add(row as usize * pitch) as *mut u32;
                for col in r.x..r.x + r.w {
                    let dst_ptr = row_ptr.add(col as usize);
                    let (dr, dg, db, da) = fmt.unpack(*dst_ptr);
                    let nr = ((color.r as u32 * color.a as u32 + dr as u32 * ia) >> 8) as u8;
                    let ng = ((color.g as u32 * color.a as u32 + dg as u32 * ia) >> 8) as u8;
                    let nb = ((color.b as u32 * color.a as u32 + db as u32 * ia) >> 8) as u8;
                    *dst_ptr = fmt.pack(nr, ng, nb, da);
                }
            }
        }
    }
}

/// Blend rendered glyph bitmaps onto the surface.
unsafe fn draw_text_surface(
    pixels: *mut u8,
    pitch: usize,
    fmt: &PixFmt,
    dt: &DrawTextCmd,
    clip: RenRect,
) {
    if dt.color.a == 0 {
        return;
    }
    let clip_x2 = clip.x + clip.w;
    let clip_y2 = clip.y + clip.h;
    let color = dt.color;

    // Group fonts: for each glyph, find which font has it.
    let mut pen_x = dt.x;
    let text_bytes = dt.text.as_bytes();
    let mut byte_pos = 0;
    while byte_pos < text_bytes.len() {
        let ch = next_char(text_bytes, &mut byte_pos);
        let cp = ch as u32;

        // Find glyph from font group (first font with a valid glyph wins).
        let (glyph, _font_height) = get_group_glyph(&dt.fonts, cp);
        let xadv = if cp == b'\t' as u32 {
            let f = dt.fonts[0].lock();
            let tab_w = f.space_advance * f.tab_size as f32;
            let r = ((pen_x - dt.x) + dt.tab_offset).rem_euclid(tab_w);
            if r == 0.0 { tab_w } else { tab_w - r }
        } else if !is_whitespace(cp) && glyph.xadvance > 0.0 {
            glyph.xadvance
        } else {
            dt.fonts[0].lock().space_advance
        };

        if let Some(ref bm) = glyph.bitmap {
            let start_x = pen_x.floor() as i32 + bm.left;
            let end_x = start_x + bm.width as i32;
            if start_x < clip_x2 && end_x > clip.x {
                let baseline = dt.fonts[0].lock().baseline;
                for row in 0..bm.rows as i32 {
                    let dst_y = row + dt.y - bm.top + baseline;
                    if dst_y < clip.y || dst_y >= clip_y2 {
                        continue;
                    }
                    let src_row = row as usize * bm.row_bytes as usize;
                    unsafe {
                        let row_ptr = pixels.add(dst_y as usize * pitch) as *mut u32;
                        for col in 0..bm.width as i32 {
                            let dst_x = start_x + col;
                            if dst_x < clip.x || dst_x >= clip_x2 {
                                continue;
                            }
                            let dst_ptr = row_ptr.add(dst_x as usize);
                            let (dr, dg, db, da) = fmt.unpack(*dst_ptr);
                            let (nr, ng, nb) = if bm.subpixel {
                                let si = src_row + col as usize * 3;
                                let sr = bm.data[si] as u32;
                                let sg = bm.data[si + 1] as u32;
                                let sb = bm.data[si + 2] as u32;
                                let ca = color.a as u32;
                                (
                                    blend_text(color.r as u32, sr, ca, dr as u32),
                                    blend_text(color.g as u32, sg, ca, dg as u32),
                                    blend_text(color.b as u32, sb, ca, db as u32),
                                )
                            } else {
                                let src = bm.data[src_row + col as usize] as u32;
                                let ca = color.a as u32;
                                (
                                    blend_text(color.r as u32, src, ca, dr as u32),
                                    blend_text(color.g as u32, src, ca, dg as u32),
                                    blend_text(color.b as u32, src, ca, db as u32),
                                )
                            };
                            *dst_ptr = fmt.pack(nr as u8, ng as u8, nb as u8, da);
                        }
                    }
                }
            }
        }

        pen_x += xadv;
    }
}

/// Text blending formula: (fc * src * fa + dst * (65025 - src * fa) + 32767) / 65025
#[inline(always)]
fn blend_text(fc: u32, src: u32, fa: u32, dst: u32) -> u32 {
    (fc * src * fa + dst * (65025 - src * fa) + 32767) / 65025
}

/// Find the glyph for `codepoint` using the font group (first font with glyph wins).
/// Returns the GlyphInfo clone and the font's height.
fn get_group_glyph(fonts: &[FontRef], codepoint: u32) -> (GlyphInfo, i32) {
    for (i, arc) in fonts.iter().enumerate() {
        let mut g = arc.lock();
        // Whitespace always uses the first font.
        if i > 0 && is_whitespace(codepoint) {
            break;
        }
        let info = g.get_glyph(codepoint).clone();
        let height = g.height;
        // If the glyph has a bitmap or an advance, use this font.
        if info.bitmap.is_some() || info.xadvance > 0.0 {
            return (info, height);
        }
    }
    // Fall back to first font's glyph (even if it's .notdef).
    let mut g = fonts[0].lock();
    let info = g.get_glyph(codepoint).clone();
    let h = g.height;
    (info, h)
}

/// Decode the next UTF-8 codepoint from `bytes` starting at `*pos`.
fn next_char(bytes: &[u8], pos: &mut usize) -> char {
    let idx = *pos;
    let b = bytes[idx];
    let byte = |offset| bytes.get(idx + offset).copied().unwrap_or(0) as u32 & 0x3F;
    let (cp, len) = if b < 0x80 {
        (b as u32, 1)
    } else if b < 0xE0 {
        (((b as u32 & 0x1F) << 6) | byte(1), 2)
    } else if b < 0xF0 {
        (((b as u32 & 0x0F) << 12) | (byte(1) << 6) | byte(2), 3)
    } else {
        (
            ((b as u32 & 0x07) << 18) | (byte(1) << 12) | (byte(2) << 6) | byte(3),
            4,
        )
    };
    *pos += len;
    char::from_u32(cp).unwrap_or('\u{FFFD}')
}
