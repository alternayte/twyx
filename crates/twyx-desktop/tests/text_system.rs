//! The macOS platform must ship a real text system. Without gpui's `font-kit`
//! feature it falls back to a no-op one that measures fake metrics and draws no
//! glyphs, and the window still opens. Runs without the test harness because
//! the macOS platform must be built on the main thread.

#[cfg(target_os = "macos")]
#[allow(dead_code, unused_imports)]
#[path = "../src/paint.rs"]
mod paint;

#[cfg(target_os = "macos")]
fn main() {
    use gpui::{TextSystem, px};

    let platform = gpui_platform::current_platform(true);
    let platform_text = platform.text_system();
    assert!(
        platform_text
            .all_font_names()
            .iter()
            .any(|name| name == "Menlo"),
        "the platform text system lists no Menlo font"
    );

    // Menlo's advance is 0.602 em, and its ascent plus descent 1.164 em.
    let metrics = paint::Metrics::measure(&TextSystem::new(platform_text), px(13.0));
    let cell = f32::from(metrics.cell_width);
    let line = f32::from(metrics.line_height);
    assert!((7.7..8.0).contains(&cell), "Menlo at 13px is {cell}px wide");
    assert!(
        (15.0..15.3).contains(&line),
        "Menlo at 13px is {line}px tall"
    );
}

#[cfg(not(target_os = "macos"))]
fn main() {}
