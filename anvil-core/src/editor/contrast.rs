//! WCAG contrast arithmetic used to keep overlay text legible under any
//! theme. A palette is free to pick colors that sit close together; the
//! helpers here take the color a theme asked for and hand back the nearest
//! variant of it that a reader can actually make out on the surface it is
//! drawn on.

use std::sync::LazyLock;

use crate::editor::types::Color;

/// WCAG 1.4.3 AA contrast ratio for body text.
pub const MIN_TEXT: f64 = 4.5;
/// WCAG 1.4.3 AA contrast ratio for large text, and 1.4.11 for the visual
/// information that identifies a UI component or one of its states.
pub const MIN_UI: f64 = 3.0;

/// Number of steps the search walks from the requested color toward the
/// pole. 64 steps land within one 8-bit channel value of the ideal blend.
const SEARCH_STEPS: u32 = 64;

const BLACK: Color = Color::new(0, 0, 0, 255);
const WHITE: Color = Color::new(255, 255, 255, 255);

/// sRGB-to-linear per channel value, precomputed because the search below
/// evaluates luminance dozens of times per color.
static LINEAR: LazyLock<[f64; 256]> = LazyLock::new(|| {
    std::array::from_fn(|i| {
        let c = i as f64 / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    })
});

/// WCAG relative luminance of a color, ignoring its alpha.
pub fn luminance(c: Color) -> f64 {
    let lin = &*LINEAR;
    0.2126 * lin[c.r as usize] + 0.7152 * lin[c.g as usize] + 0.0722 * lin[c.b as usize]
}

/// WCAG contrast ratio between two opaque colors, from 1.0 to 21.0.
pub fn ratio(a: Color, b: Color) -> f64 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// `fg` composited over the opaque `bg` using `fg`'s alpha.
pub fn flatten(fg: Color, bg: Color) -> Color {
    if fg.a == 255 {
        return Color::new(fg.r, fg.g, fg.b, 255);
    }
    let a = fg.a as f64 / 255.0;
    let mix = |f: u8, b: u8| (f as f64 * a + b as f64 * (1.0 - a)).round() as u8;
    Color::new(mix(fg.r, bg.r), mix(fg.g, bg.g), mix(fg.b, bg.b), 255)
}

fn blend(from: Color, to: Color, t: f64) -> Color {
    let mix = |f: u8, o: u8| (f as f64 + (o as f64 - f as f64) * t).round() as u8;
    Color::new(
        mix(from.r, to.r),
        mix(from.g, to.g),
        mix(from.b, to.b),
        from.a,
    )
}

/// `fg` moved toward black or white by the smallest amount that reaches
/// `min_ratio` against `bg`, keeping the theme's hue as far as the target
/// allows. A color that already clears the threshold is returned untouched,
/// so a palette with contrast keeps exactly the colors it chose.
///
/// `fg`'s alpha is preserved: the search runs against the flattened color so
/// a translucent overlay tint is judged as the reader sees it.
pub fn readable(fg: Color, bg: Color, min_ratio: f64) -> Color {
    let base = flatten(bg, BLACK);
    let flat = flatten(fg, base);
    if ratio(flat, base) >= min_ratio {
        return fg;
    }
    // Whichever pole the background is furthest from is the only one that
    // can reach a high ratio at all; against a mid-tone neither may, in
    // which case the search still returns the best available end point.
    let pole = if ratio(WHITE, base) >= ratio(BLACK, base) {
        WHITE
    } else {
        BLACK
    };
    // Contrast is not monotonic along this path when `fg` sits on the same
    // side of `bg` as the pole - it dips through 1.0 as the blend crosses
    // the background's luminance - so walk out from the requested color and
    // stop at the first step that qualifies rather than bisecting.
    for step in 1..=SEARCH_STEPS {
        let t = step as f64 / SEARCH_STEPS as f64;
        let candidate = blend(fg, pole, t);
        if ratio(flatten(candidate, base), base) >= min_ratio {
            return candidate;
        }
    }
    Color::new(pole.r, pole.g, pole.b, fg.a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratio_of_black_and_white_is_the_wcag_maximum() {
        assert!((ratio(BLACK, WHITE) - 21.0).abs() < 0.01);
    }

    #[test]
    fn ratio_of_a_color_with_itself_is_one() {
        let c = Color::new(0x2c, 0x2a, 0x2b, 255);
        assert!((ratio(c, c) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn readable_leaves_a_color_that_already_passes_untouched() {
        let bg = Color::new(0xff, 0xff, 0xff, 255);
        let fg = Color::new(0x1f, 0x1f, 0x1f, 255);
        assert_eq!(readable(fg, bg, MIN_TEXT), fg);
    }

    #[test]
    fn readable_darkens_a_pale_color_on_a_light_background() {
        let bg = Color::new(0xec, 0xec, 0xec, 255);
        let fg = Color::new(0xd0, 0xd0, 0xd0, 255);
        let out = readable(fg, bg, MIN_TEXT);
        assert!(ratio(out, bg) >= MIN_TEXT);
        assert!(luminance(out) < luminance(fg));
    }

    #[test]
    fn readable_lightens_a_dark_color_on_a_dark_background() {
        let bg = Color::new(0x1f, 0x1f, 0x1f, 255);
        let fg = Color::new(0x2b, 0x2b, 0x2b, 255);
        let out = readable(fg, bg, MIN_TEXT);
        assert!(ratio(out, bg) >= MIN_TEXT);
        assert!(luminance(out) > luminance(fg));
    }

    #[test]
    fn readable_keeps_the_hue_it_started_from() {
        let bg = Color::new(0xec, 0xec, 0xec, 255);
        let fg = Color::new(0xad, 0xd6, 0xff, 255);
        let out = readable(fg, bg, MIN_UI);
        assert!(out.b > out.r, "blue should stay the dominant channel");
    }

    #[test]
    fn readable_reaches_the_threshold_for_every_gray_pairing() {
        for bg in (0..=255).step_by(5) {
            for fg in (0..=255).step_by(15) {
                let b = Color::new(bg, bg, bg, 255);
                let f = Color::new(fg, fg, fg, 255);
                let out = readable(f, b, MIN_TEXT);
                assert!(
                    ratio(out, b) >= MIN_TEXT - 0.05,
                    "fg {fg} on bg {bg} reached only {}",
                    ratio(out, b)
                );
            }
        }
    }

    #[test]
    fn flatten_composites_a_translucent_color_onto_its_background() {
        let bg = Color::new(0, 0, 0, 255);
        let fg = Color::new(255, 255, 255, 128);
        let out = flatten(fg, bg);
        assert_eq!(out.a, 255);
        assert!((out.r as i32 - 128).abs() <= 1);
    }
}
