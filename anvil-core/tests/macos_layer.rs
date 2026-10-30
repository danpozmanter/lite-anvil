//! The editor window's layer on macOS: partial frames carry the rest of the
//! window forward, a resize repaints in full, and released buffers rebuild.
//! Runs on the main thread (`harness = false`) because Cocoa requires it.

#[cfg(target_os = "macos")]
fn main() {
    use anvil_core::editor::draw_context::NativeDrawContext;
    use anvil_core::editor::view::DrawContext;
    use anvil_core::{renderer, window};

    const BLACK: [u8; 4] = [0, 0, 0, 255];
    const COLORS: [[u8; 4]; 4] = [
        [200, 10, 10, 255],
        [10, 200, 10, 255],
        [10, 10, 200, 255],
        [200, 200, 10, 255],
    ];

    fn quadrant(i: usize, w: i32, h: i32) -> (i32, i32, i32, i32) {
        let (qw, qh) = (w / 2, h / 2);
        let x = if i % 2 == 0 { 0 } else { qw };
        let y = if i / 2 == 0 { 0 } else { qh };
        (
            x,
            y,
            if i % 2 == 0 { qw } else { w - qw },
            if i / 2 == 0 { qh } else { h - qh },
        )
    }

    fn frame(colors: [[u8; 4]; 4]) {
        let (w, h) = window::get_drawable_size();
        renderer::native_begin_frame();
        let mut ctx = NativeDrawContext::new();
        for (i, color) in colors.into_iter().enumerate() {
            let (x, y, qw, qh) = quadrant(i, w, h);
            ctx.draw_rect(
                f64::from(x),
                f64::from(y),
                f64::from(qw),
                f64::from(qh),
                color,
            );
        }
        renderer::native_end_frame();
    }

    fn check(colors: [[u8; 4]; 4], when: &str) {
        let shown = renderer::capture_window()
            .unwrap_or_else(|err| panic!("{when}: capturing the layer: {err}"))
            .unwrap_or_else(|| panic!("{when}: the layer shows no frame"));
        let (w, h) = window::get_drawable_size();
        assert_eq!((shown.width, shown.height), (w, h), "{when}: frame size");
        for (i, color) in colors.into_iter().enumerate() {
            let (x, y, qw, qh) = quadrant(i, w, h);
            for (px, py) in [(x, y), (x + qw / 2, y + qh / 2), (x + qw - 1, y + qh - 1)] {
                let at = (py * w + px) as usize * 4;
                let bgra = &shown.pixels[at..at + 4];
                assert_eq!(
                    [bgra[2], bgra[1], bgra[0]],
                    [color[0], color[1], color[2]],
                    "{when}: quadrant {i} at ({px}, {py})"
                );
            }
        }
    }

    window::init().expect("SDL video init");
    window::create_window("lite-anvil layer check").expect("creating the window");

    frame([BLACK; 4]);
    check([BLACK; 4], "first frame");

    // Each quadrant changes alone, a frame apiece, so each frame's damage is
    // that quadrant and the others reach the painted buffer by copying.
    let mut colors = [BLACK; 4];
    for (i, color) in COLORS.into_iter().enumerate() {
        colors[i] = color;
        frame(colors);
        check(colors, &format!("after repainting quadrant {i}"));
    }

    window::set_window_size(800, 500, -1, -1);
    let rotated = [COLORS[3], COLORS[0], COLORS[1], COLORS[2]];
    frame(rotated);
    check(rotated, "after the resize");

    renderer::drop_caches();
    check(rotated, "after trimming the pool");
    frame(COLORS);
    check(COLORS, "after trimming and repainting");

    renderer::drop_hidden_caches();
    assert!(
        renderer::capture_window()
            .expect("capturing the layer")
            .is_none(),
        "a hidden window's layer holds no frame"
    );
    frame(rotated);
    check(rotated, "after releasing every buffer");

    window::shutdown();
    println!("macos_layer: ok");
}

#[cfg(not(target_os = "macos"))]
fn main() {}
