//! Renders the desktop window to PNG files without a screen: GPUI's headless
//! renderer draws the real window, driven by keys and clicks, against a real
//! daemon. For a person to look at, where no screen can be captured: over
//! SSH, on a locked screen, in CI.
//!
//!     TWYX_BINARY=target/debug/twyx cargo run -p twyx-desktop --example scenes -- <dir>
//!
//! Point `XDG_RUNTIME_DIR`, `XDG_STATE_HOME` and `XDG_CONFIG_HOME` at empty
//! directories, or the window attaches to your own daemon and changes your
//! layout.

#[allow(dead_code)]
#[path = "../src/keys.rs"]
mod keys;
#[allow(dead_code, unused_imports)]
#[path = "../src/paint.rs"]
mod paint;
#[allow(dead_code)]
#[path = "../src/panes.rs"]
mod panes;
#[allow(dead_code)]
#[path = "../src/terminal/mod.rs"]
mod terminal;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use gpui::{
    AnyWindowHandle, AppContext, HeadlessAppContext, Keystroke, Modifiers, MouseButton,
    MouseDownEvent, MouseUpEvent, PlatformInput, point, px, size,
};

/// Lets the window and the daemon run for `wait`, then draws a frame.
fn pump(cx: &mut HeadlessAppContext, window: AnyWindowHandle, wait: Duration) -> Result<()> {
    let end = Instant::now() + wait;
    while Instant::now() < end {
        cx.advance_clock(Duration::from_millis(10));
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(10));
    }
    cx.update_window(window, |_, window, _| window.refresh())?;
    cx.run_until_parked();
    Ok(())
}

fn keys(cx: &mut HeadlessAppContext, window: AnyWindowHandle, keys: &[&str]) -> Result<()> {
    for key in keys {
        let keystroke = Keystroke::parse(key).with_context(|| format!("parse key {key}"))?;
        cx.update_window(window, |_, window, cx| {
            window.dispatch_keystroke(keystroke, cx);
        })?;
        pump(cx, window, Duration::from_millis(150))?;
    }
    Ok(())
}

fn click(cx: &mut HeadlessAppContext, window: AnyWindowHandle, x: f32, y: f32) -> Result<()> {
    let position = point(px(x), px(y));
    cx.update_window(window, |_, window, cx| {
        window.dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent {
                button: MouseButton::Left,
                position,
                modifiers: Modifiers::default(),
                click_count: 1,
                first_mouse: false,
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::MouseUp(MouseUpEvent {
                button: MouseButton::Left,
                position,
                modifiers: Modifiers::default(),
                click_count: 1,
            }),
            cx,
        );
    })
}

fn shot(
    cx: &mut HeadlessAppContext,
    window: AnyWindowHandle,
    out: &Path,
    name: &str,
) -> Result<()> {
    pump(cx, window, Duration::from_millis(600))?;
    let path = out.join(format!("{name}.png"));
    cx.capture_screenshot(window)?
        .save(&path)
        .with_context(|| format!("write {}", path.display()))?;
    println!("{}", path.display());
    Ok(())
}

fn main() -> Result<()> {
    let out = std::path::PathBuf::from(std::env::args().nth(1).context("an output directory")?);
    let text = gpui_platform::current_platform(true).text_system();
    let mut cx = HeadlessAppContext::with_platform(
        text,
        Arc::new(()),
        gpui_platform::current_headless_renderer,
    );
    cx.update(|cx| cx.set_reduce_motion(true));
    let keymap = twyx_core::keys::Keymap::default();
    let window: AnyWindowHandle = cx
        .open_window(size(px(1280.0), px(800.0)), |window, cx| {
            cx.new(|cx| terminal::Terminal::new(window, cx, keymap))
        })?
        .into();
    let cx = &mut cx;
    pump(cx, window, Duration::from_secs(3))?;
    shot(cx, window, &out, "1-one-pane")?;
    keys(cx, window, &["ctrl-g", "n"])?;
    keys(cx, window, &["ctrl-g", "p", "d", "escape"])?;
    shot(cx, window, &out, "2-three-panes")?;
    click(cx, window, 100.0, 300.0)?;
    shot(cx, window, &out, "3-clicked-left")?;
    keys(cx, window, &["ctrl-g", "z"])?;
    shot(cx, window, &out, "4-zoom")?;
    keys(cx, window, &["ctrl-g", "z"])?;
    keys(cx, window, &["ctrl-g", "p", "f", "escape"])?;
    shot(cx, window, &out, "5-floating")?;
    keys(cx, window, &["ctrl-g", "t", "n", "escape"])?;
    shot(cx, window, &out, "6-new-tab")?;
    keys(cx, window, &["ctrl-g", "?"])?;
    shot(cx, window, &out, "7-keys")
}
