use super::fallback;
use super::font::{Antialiasing, FontRef, GlyphInfo, Hinting, is_whitespace};
use sdl3_sys::everything::*;

// ── Constants ─────────────────────────────────────────────────────────────────

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
    /// Shared-ownership slice of the font group. Cloning this per
    /// command is a single atomic refcount bump - the per-call
    /// `Vec<FontRef>` alloc the old shape required (thousands per
    /// frame on a code view) is gone.
    pub fonts: std::sync::Arc<[FontRef]>,
    /// Byte range of this run's text inside the cache's per-frame arena, so a
    /// command carries two integers instead of owning a heap allocation.
    pub text_off: u32,
    pub text_len: u32,
    pub x: f32,
    pub y: i32,
    pub color: RenColor,
    /// Distance from the line's left edge to x; used for tab-stop alignment.
    pub tab_offset: f32,
    /// Bounding rect used for overlap check (pre-computed).
    pub bounding: RenRect,
}

/// An RGBA image to blit onto the surface.
pub struct DrawImageCmd {
    pub data: std::sync::Arc<Vec<u8>>,
    pub width: i32,
    pub height: i32,
    pub x: i32,
    pub y: i32,
}

pub enum Command {
    SetClip(RenRect),
    DrawRect { rect: RenRect, color: RenColor },
    DrawText(DrawTextCmd),
    DrawImage(DrawImageCmd),
}

// ── RenCache ──────────────────────────────────────────────────────────────────

pub struct RenCache {
    pub commands: Vec<Command>,
    /// Per-frame scratch holding every draw-text run's bytes; commands index
    /// into it by (offset, len). Cleared each `begin_frame`, so one growable
    /// buffer replaces a heap allocation per text run.
    pub text_arena: String,
    /// Per-cell hash grid sized to the live surface, row-major over
    /// `cells_w` x `cells_h`. Indexed as `y * cells_w + x`.
    cells: Vec<u32>,
    cells_prev: Vec<u32>,
    /// Cell-grid dimensions, recomputed each frame from the surface size so
    /// the grid always covers the full surface (8K and multi-monitor pixel
    /// extents included) rather than a fixed pixel ceiling.
    cells_w: usize,
    cells_h: usize,
    pub screen: RenRect,
    pub last_clip: RenRect,
}

/// Number of `CELL_SIZE`-wide cells needed to cover `px` pixels (ceiling).
fn cells_for(px: i32) -> usize {
    let px = px.max(0) as usize;
    px.div_ceil(CELL_SIZE as usize)
}

impl RenCache {
    pub fn new() -> Self {
        // The grid is allocated on the first `begin_frame` once the surface
        // size is known; until then it is empty and the initial frame paints
        // fully (the resize path forces a full invalidate).
        RenCache {
            commands: Vec::new(),
            text_arena: String::new(),
            cells: Vec::new(),
            cells_prev: Vec::new(),
            cells_w: 0,
            cells_h: 0,
            screen: RenRect::default(),
            last_clip: RenRect::default(),
        }
    }

    pub fn invalidate(&mut self) {
        self.cells_prev.fill(0xFF_FF_FF_FF);
    }

    pub fn begin_frame(&mut self, w: i32, h: i32) {
        let resized = self.screen.w != w || self.screen.h != h;
        if resized {
            self.screen = RenRect { x: 0, y: 0, w, h };
        }
        let cells_w = cells_for(w);
        let cells_h = cells_for(h);
        if cells_w != self.cells_w || cells_h != self.cells_h {
            // Surface grew or shrank past a cell boundary: resize the grid and
            // repaint everything this frame. `cells_prev` of all-ones marks
            // every cell dirty against this frame's freshly hashed cells.
            self.cells_w = cells_w;
            self.cells_h = cells_h;
            let len = cells_w * cells_h;
            self.cells = vec![HASH_INITIAL; len];
            self.cells_prev = vec![0xFF_FF_FF_FF; len];
        } else {
            // Reset cells to HASH_INITIAL for this frame's hash accumulation.
            self.cells.fill(HASH_INITIAL);
            if resized {
                self.invalidate();
            }
        }
        self.last_clip = self.screen;
        self.commands.clear();
        self.text_arena.clear();
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

    /// Push a DrawImage (RGBA bitmap) command.
    pub fn push_draw_image(
        &mut self,
        data: std::sync::Arc<Vec<u8>>,
        width: i32,
        height: i32,
        x: i32,
        y: i32,
    ) {
        let rect = RenRect {
            x,
            y,
            w: width,
            h: height,
        };
        if self.last_clip.overlaps(rect) {
            self.commands.push(Command::DrawImage(DrawImageCmd {
                data,
                width,
                height,
                x,
                y,
            }));
        }
    }

    /// Push a DrawText command. Returns the new x position after the text.
    /// Takes the font slice and text by shared/owned handles so the
    /// caller does not re-allocate per call.
    pub fn push_draw_text(
        &mut self,
        fonts: std::sync::Arc<[FontRef]>,
        text: &str,
        x: f32,
        y: i32,
        color: RenColor,
        tab_offset: f32,
    ) -> f32 {
        let Some(first) = fonts.first() else {
            return x;
        };
        let width = group_text_width(&fonts, text, tab_offset);
        let height = first.lock().height;
        let bounding = RenRect {
            x: x as i32,
            y,
            w: width as i32,
            h: height,
        };
        if self.last_clip.overlaps(bounding) {
            let text_off = self.text_arena.len() as u32;
            self.text_arena.push_str(text);
            let text_len = text.len() as u32;
            self.commands.push(Command::DrawText(DrawTextCmd {
                fonts,
                text_off,
                text_len,
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
            let (rect, h) = cmd_hash(cmd, &self.text_arena);
            if rect.is_empty() {
                continue;
            }
            let clipped = rect.intersect(self.screen);
            if clipped.is_empty() {
                continue;
            }
            update_cells(&mut self.cells, self.cells_w, self.cells_h, clipped, h);
        }

        // Find changed cells → dirty rects.
        let mut dirty: Vec<RenRect> = Vec::new();
        for cy in 0..self.cells_h {
            for cx in 0..self.cells_w {
                let idx = cx + cy * self.cells_w;
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
fn cmd_hash(cmd: &Command, arena: &str) -> (RenRect, u32) {
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
            let text = arena
                .get(dt.text_off as usize..(dt.text_off as usize + dt.text_len as usize))
                .unwrap_or("");
            fnv1a_update(&mut h, text.as_bytes());
            fnv1a_update(&mut h, &dt.x.to_bits().to_ne_bytes());
            fnv1a_update(&mut h, &dt.y.to_ne_bytes());
            fnv1a_update(&mut h, &[dt.color.r, dt.color.g, dt.color.b, dt.color.a]);
            (r, h)
        }
        Command::DrawImage(di) => {
            let r = RenRect {
                x: di.x,
                y: di.y,
                w: di.width,
                h: di.height,
            };
            fnv1a_update(&mut h, &di.x.to_ne_bytes());
            fnv1a_update(&mut h, &di.y.to_ne_bytes());
            fnv1a_update(
                &mut h,
                &(std::sync::Arc::as_ptr(&di.data) as usize).to_ne_bytes(),
            );
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

fn update_cells(cells: &mut [u32], cells_w: usize, cells_h: usize, r: RenRect, h: u32) {
    if cells_w == 0 || cells_h == 0 {
        return;
    }
    let x1 = (r.x / CELL_SIZE) as usize;
    let y1 = (r.y / CELL_SIZE) as usize;
    let x2 = ((r.x + r.w) / CELL_SIZE) as usize;
    let y2 = ((r.y + r.h) / CELL_SIZE) as usize;
    let h_bytes = h.to_ne_bytes();
    for cy in y1..=y2.min(cells_h - 1) {
        for cx in x1..=x2.min(cells_w - 1) {
            let idx = cx + cy * cells_w;
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
    arena: &str,
    dirty: &[RenRect],
) {
    if dirty.is_empty() {
        return;
    }

    // Honor SDL's surface-lock contract before touching raw pixels. The window
    // surfaces this editor uses are lock-free on every shipped backend, so this
    // is a no-op in practice, but a backend that sets SDL_MUSTLOCK would
    // otherwise have us write through an invalid or stale pixel pointer. The
    // pixel pointer is read only after the lock succeeds, since locking can
    // (re)establish it.
    let must_lock = unsafe { SDL_MUSTLOCK(surface) };
    if must_lock && !unsafe { SDL_LockSurface(surface) } {
        return;
    }

    let prepared = unsafe {
        let details = SDL_GetPixelFormatDetails((*surface).format);
        let fmt = PixFmt::from_sdl(details);
        let pitch = (*surface).pitch as usize;
        let pixels = (*surface).pixels as *mut u8;
        if pixels.is_null() {
            None
        } else {
            // SDL_GetWindowSizeInPixels (used for screen.h in begin_frame) can
            // differ from the actual surface dimensions when the window manager
            // hasn't yet applied a resize request.  Clamp all pixel access to
            // the real surface bounds so we never walk off the end of the
            // buffer.
            let bounds = RenRect {
                x: 0,
                y: 0,
                w: (*surface).w,
                h: (*surface).h,
            };
            Some((fmt, pitch, pixels, bounds))
        }
    };

    if let Some((fmt, pitch, pixels, surface_bounds)) = prepared {
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
                        let text = arena
                            .get(
                                dt.text_off as usize..(dt.text_off as usize + dt.text_len as usize),
                            )
                            .unwrap_or("");
                        unsafe { draw_text_surface(pixels, pitch, &fmt, dt, text, clip) };
                    }
                    Command::DrawImage(di) => {
                        unsafe { draw_image_surface(pixels, pitch, &fmt, di, clip) };
                    }
                }
            }
        }

        unsafe { SDL_SetSurfaceClipRect(surface, std::ptr::null()) };
    }

    if must_lock {
        unsafe { SDL_UnlockSurface(surface) };
    }
}

/// Alpha-blend an RGBA image onto the surface.
unsafe fn draw_image_surface(
    pixels: *mut u8,
    pitch: usize,
    fmt: &PixFmt,
    di: &DrawImageCmd,
    clip: RenRect,
) {
    let clip_x2 = clip.x + clip.w;
    let clip_y2 = clip.y + clip.h;
    let data = &di.data;
    let src_stride = di.width as usize * 4;
    for row in 0..di.height {
        let dst_y = di.y + row;
        if dst_y < clip.y || dst_y >= clip_y2 {
            continue;
        }
        let src_row = row as usize * src_stride;
        unsafe {
            let row_ptr = pixels.add(dst_y as usize * pitch) as *mut u32;
            for col in 0..di.width {
                let dst_x = di.x + col;
                if dst_x < clip.x || dst_x >= clip_x2 {
                    continue;
                }
                let si = src_row + col as usize * 4;
                let sr = data[si] as u32;
                let sg = data[si + 1] as u32;
                let sb = data[si + 2] as u32;
                let sa = data[si + 3] as u32;
                if sa == 0 {
                    continue;
                }
                let dst_ptr = row_ptr.add(dst_x as usize);
                if sa == 255 {
                    *dst_ptr = fmt.pack(sr as u8, sg as u8, sb as u8, 255);
                } else {
                    let (dr, dg, db, da) = fmt.unpack(*dst_ptr);
                    let ia = 255 - sa;
                    let nr = ((sr * sa + dr as u32 * ia) >> 8) as u8;
                    let ng = ((sg * sa + dg as u32 * ia) >> 8) as u8;
                    let nb = ((sb * sa + db as u32 * ia) >> 8) as u8;
                    *dst_ptr = fmt.pack(nr, ng, nb, da);
                }
            }
        }
    }
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
    text: &str,
    clip: RenRect,
) {
    if dt.color.a == 0 {
        return;
    }
    let Some(first_font) = dt.fonts.first() else {
        return;
    };
    let clip_x2 = clip.x + clip.w;
    let clip_y2 = clip.y + clip.h;
    let color = dt.color;

    // The whole run shares one font for baseline/spacing metrics, so read them
    // once under a single lock instead of re-locking per glyph and per
    // whitespace character.
    let (baseline, space_advance, tab_w) = {
        let f = first_font.lock();
        (
            f.baseline,
            f.space_advance,
            f.space_advance * f.tab_size as f32,
        )
    };

    // Group fonts: for each glyph, find which font has it.
    let mut pen_x = dt.x;
    let text_bytes = text.as_bytes();
    let mut byte_pos = 0;
    while byte_pos < text_bytes.len() {
        let ch = next_char(text_bytes, &mut byte_pos);
        let cp = ch as u32;

        // Find glyph from font group (first font with a valid glyph wins).
        let (glyph, _font_height) = get_group_glyph(&dt.fonts, cp);
        let xadv = if cp == b'\t' as u32 {
            let r = ((pen_x - dt.x) + dt.tab_offset).rem_euclid(tab_w);
            if r == 0.0 { tab_w } else { tab_w - r }
        } else if !is_whitespace(cp) && glyph.xadvance > 0.0 {
            glyph.xadvance
        } else {
            space_advance
        };

        if let Some(ref bm) = glyph.bitmap {
            let start_x = pen_x.floor() as i32 + bm.left;
            let end_x = start_x + bm.width as i32;
            if start_x < clip_x2 && end_x > clip.x {
                // Clamp the column span to the clip rect once so the inner loop
                // has no per-pixel branch and advances its source index
                // linearly. dst_x = start_x + col, clipped to [clip.x, clip_x2).
                let col_start = (clip.x - start_x).max(0);
                let col_end = (clip_x2 - start_x).min(bm.width as i32);
                let y_base = dt.y - bm.top + baseline;
                for row in 0..bm.rows as i32 {
                    let dst_y = row + y_base;
                    if dst_y < clip.y || dst_y >= clip_y2 {
                        continue;
                    }
                    let src_row = row as usize * bm.row_bytes as usize;
                    unsafe {
                        let row_ptr = pixels.add(dst_y as usize * pitch) as *mut u32;
                        for col in col_start..col_end {
                            let dst_x = start_x + col;
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

/// True when the font actually maps the codepoint and produced a drawable glyph.
fn glyph_usable(info: &GlyphInfo) -> bool {
    info.defined && (info.bitmap.is_some() || info.xadvance > 0.0)
}

/// Find the glyph for `codepoint` using the font group (first font with glyph
/// wins), then the system fallback fonts. Returns the GlyphInfo clone and the
/// chosen font's height; if nothing covers the codepoint, the first font's
/// .notdef box. Precondition: `fonts` is non-empty.
fn get_group_glyph(fonts: &[FontRef], codepoint: u32) -> (GlyphInfo, i32) {
    let (first, height, size, aa, hinting) = {
        let mut g = fonts[0].lock();
        (
            g.get_glyph(codepoint).clone(),
            g.height,
            g.size,
            g.antialiasing,
            g.hinting,
        )
    };
    // Whitespace always uses the first font.
    if is_whitespace(codepoint) || glyph_usable(&first) {
        return (first, height);
    }
    for font in &fonts[1..] {
        let mut g = font.lock();
        let info = g.get_glyph(codepoint);
        if glyph_usable(info) {
            return (info.clone(), g.height);
        }
    }
    fallback::system_glyph(codepoint, size, aa, hinting).unwrap_or_else(|| {
        fallback::note_uncovered(codepoint);
        (first, height)
    })
}

/// Advance width of `codepoint` when the primary font does not cover it: walk
/// the fallback fonts, then the installed system fallbacks, mirroring
/// `get_group_glyph`'s selection. `primary_advance` is the primary font's own
/// advance, used as the last-resort width when nothing covers the codepoint.
fn group_advance_fallback(
    fonts: &[FontRef],
    codepoint: u32,
    primary_advance: f32,
    size: f32,
    aa: Antialiasing,
    hinting: Hinting,
) -> f32 {
    for font in &fonts[1..] {
        let mut g = font.lock();
        let info = g.get_glyph(codepoint);
        if glyph_usable(info) {
            return info.xadvance;
        }
    }
    match fallback::system_glyph(codepoint, size, aa, hinting) {
        Some((glyph, _)) => glyph.xadvance,
        None => {
            fallback::note_uncovered(codepoint);
            primary_advance
        }
    }
}

/// Compute the rendered width of `text` in pixels using the full font group,
/// matching the per-glyph advances `draw_text_surface` applies.
pub(crate) fn group_text_width(fonts: &[FontRef], text: &str, tab_offset: f32) -> f32 {
    let Some(first) = fonts.first() else {
        return 0.0;
    };
    // Hold one lock on the primary font for the whole measurement run: it
    // covers the common case (printable code), so every covered glyph resolves
    // and bumps its LRU tick under this single guard instead of re-locking per
    // character. Only an uncovered codepoint drops to the fallback fonts, which
    // take their own locks.
    let mut primary = first.lock();
    let space_advance = primary.space_advance;
    let tab_w = space_advance * primary.tab_size as f32;
    let mut w = 0.0f32;
    for ch in text.chars() {
        let cp = ch as u32;
        if cp == b'\t' as u32 {
            let r = (w + tab_offset).rem_euclid(tab_w);
            w += if r == 0.0 { tab_w } else { tab_w - r };
            continue;
        }
        if is_whitespace(cp) {
            w += space_advance;
            continue;
        }
        let info = primary.get_glyph(cp);
        let adv = if glyph_usable(info) {
            info.xadvance
        } else {
            let primary_advance = info.xadvance;
            let (size, aa, hinting) = (primary.size, primary.antialiasing, primary.hinting);
            group_advance_fallback(fonts, cp, primary_advance, size, aa, hinting)
        };
        w += if adv > 0.0 { adv } else { space_advance };
    }
    w
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

#[cfg(test)]
mod tests {
    use super::super::font::{Antialiasing, FontInner, Hinting};
    use super::*;
    use parking_lot::Mutex;
    use std::sync::Arc;

    fn load(name: &str) -> FontRef {
        let path = format!("{}/../data/fonts/{}", env!("CARGO_MANIFEST_DIR"), name);
        let inner = FontInner::load(&path, 15.0, Antialiasing::Grayscale, Hinting::Slight)
            .expect("bundled font loads");
        Arc::new(Mutex::new(inner))
    }

    /// First private-use-area codepoint mapped by seti.ttf but not by Lilex.
    fn pua_codepoint(lilex: &FontRef, seti: &FontRef) -> u32 {
        (0xE000..0xF8FF)
            .find(|&cp| seti.lock().get_glyph(cp).defined && !lilex.lock().get_glyph(cp).defined)
            .expect("seti.ttf maps private-use codepoints Lilex lacks")
    }

    #[test]
    fn glyph_defined_reflects_cmap_coverage() {
        let lilex = load("Lilex-Regular.ttf");
        let mut g = lilex.lock();
        assert!(g.get_glyph('A' as u32).defined);
        assert!(!g.get_glyph('中' as u32).defined);
    }

    #[test]
    fn group_glyph_falls_back_past_notdef() {
        let lilex = load("Lilex-Regular.ttf");
        let seti = load("seti.ttf");
        let cp = pua_codepoint(&lilex, &seti);
        let fonts = [lilex, seti];
        let (glyph, _) = get_group_glyph(&fonts, cp);
        assert!(glyph.defined, "group lookup must skip the .notdef box");
    }

    #[test]
    fn group_text_width_uses_fallback_advance() {
        let lilex = load("Lilex-Regular.ttf");
        let seti = load("seti.ttf");
        let cp = pua_codepoint(&lilex, &seti);
        let expected = seti.lock().get_glyph(cp).xadvance;
        let fonts = [lilex, seti];
        let text = char::from_u32(cp).unwrap().to_string();
        assert_eq!(group_text_width(&fonts, &text, 0.0), expected);
    }

    #[test]
    fn group_glyph_returns_notdef_when_nothing_covers() {
        let lilex = load("Lilex-Regular.ttf");
        let fonts = [lilex];
        // U+FFFFE is a noncharacter no font maps.
        let (glyph, _) = get_group_glyph(&fonts, 0xFFFFE);
        assert!(!glyph.defined);
    }

    #[test]
    fn empty_font_group_measures_zero_width() {
        let fonts: Vec<FontRef> = Vec::new();
        assert_eq!(group_text_width(&fonts, "abc", 0.0), 0.0);
    }

    #[test]
    fn push_draw_text_empty_group_is_noop() {
        let mut cache = RenCache::new();
        cache.begin_frame(800, 600);
        let fonts: Arc<[FontRef]> = Arc::from(Vec::<FontRef>::new());
        let x = cache.push_draw_text(fonts, "hi", 10.0, 20, RenColor::default(), 0.0);
        assert_eq!(x, 10.0);
        assert!(cache.commands.is_empty());
    }

    #[test]
    fn uncovered_codepoints_are_reported_once() {
        let lilex = load("Lilex-Regular.ttf");
        let fonts = [lilex];
        let (_, _) = get_group_glyph(&fonts, 0xFFFFE);
        assert_eq!(fallback::take_uncovered(), vec![0xFFFFE]);
        let (_, _) = get_group_glyph(&fonts, 0xFFFFE);
        assert!(fallback::take_uncovered().is_empty());
    }
}
