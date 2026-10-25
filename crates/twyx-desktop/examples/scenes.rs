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
#[path = "../src/highlight.rs"]
mod highlight;
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
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, PlatformInput, Point, point, px, size,
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

/// One mouse event at a window point.
fn mouse(cx: &mut HeadlessAppContext, window: AnyWindowHandle, event: PlatformInput) -> Result<()> {
    cx.update_window(window, |_, window, cx| {
        window.dispatch_event(event, cx);
    })?;
    pump(cx, window, Duration::from_millis(150))
}

fn down(at: Point<Pixels>) -> PlatformInput {
    PlatformInput::MouseDown(MouseDownEvent {
        button: MouseButton::Left,
        position: at,
        modifiers: Modifiers::default(),
        click_count: 1,
        first_mouse: false,
    })
}

fn drag_to(at: Point<Pixels>) -> PlatformInput {
    PlatformInput::MouseMove(MouseMoveEvent {
        position: at,
        pressed_button: Some(MouseButton::Left),
        modifiers: Modifiers::default(),
    })
}

fn up(at: Point<Pixels>) -> PlatformInput {
    PlatformInput::MouseUp(MouseUpEvent {
        button: MouseButton::Left,
        position: at,
        modifiers: Modifiers::default(),
        click_count: 1,
    })
}

/// A point in the window, `x` from the tab area's left edge, right of the
/// docked sidebar.
fn at(x: f32, y: f32) -> Point<Pixels> {
    point(px(twyx_core::theme::DESKTOP.sidebar + x), px(y))
}

fn click(cx: &mut HeadlessAppContext, window: AnyWindowHandle, at: Point<Pixels>) -> Result<()> {
    mouse(cx, window, down(at))?;
    mouse(cx, window, up(at))
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
            cx.new(|cx| terminal::Terminal::new(window, cx, keymap, None))
        })?
        .into();
    let cx = &mut cx;
    pump(cx, window, Duration::from_secs(3))?;
    // `review` as the second argument renders only the review scenes.
    if std::env::args().nth(2).as_deref() == Some("review") {
        return review(cx, window, &out);
    }
    shot(cx, window, &out, "1-one-pane")?;
    keys(cx, window, &["ctrl-g", "n"])?;
    keys(cx, window, &["ctrl-g", "p", "d", "escape"])?;
    shot(cx, window, &out, "2-three-panes")?;
    click(cx, window, at(100.0, 300.0))?;
    shot(cx, window, &out, "3-clicked-left")?;
    // The bottom right pane by its title, to the left pane's left edge.
    mouse(cx, window, down(at(830.0, 410.0)))?;
    mouse(cx, window, drag_to(at(30.0, 300.0)))?;
    shot(cx, window, &out, "4-moving")?;
    mouse(cx, window, up(at(30.0, 300.0)))?;
    shot(cx, window, &out, "5-dropped-beside")?;
    // The right pane by its title, to the middle of the leftmost one.
    mouse(cx, window, down(at(900.0, 40.0)))?;
    mouse(cx, window, drag_to(at(160.0, 400.0)))?;
    shot(cx, window, &out, "6-moving-to-stack")?;
    mouse(cx, window, up(at(160.0, 400.0)))?;
    shot(cx, window, &out, "7-stacked")?;
    keys(cx, window, &["ctrl-g", "z"])?;
    shot(cx, window, &out, "8-zoom")?;
    keys(cx, window, &["ctrl-g", "z"])?;
    keys(cx, window, &["ctrl-g", "p", "f", "escape"])?;
    shot(cx, window, &out, "9-floating")?;
    keys(cx, window, &["ctrl-g", "t", "n", "escape"])?;
    shot(cx, window, &out, "10-new-tab")?;

    // The palette narrows as you type; enter runs the choice.
    let (palette, copy) = if cfg!(target_os = "macos") {
        (&["cmd-shift-p"][..], "cmd-c")
    } else {
        (&["ctrl-g", ":"][..], "ctrl-shift-c")
    };
    keys(cx, window, palette)?;
    keys(cx, window, &["s", "p", "l", "i", "t", " ", "d"])?;
    shot(cx, window, &out, "10a-palette")?;
    keys(cx, window, &["enter"])?;
    shot(cx, window, &out, "10b-palette-ran")?;
    keys(cx, window, &["ctrl-g", "s"])?;
    shot(cx, window, &out, "10c-switcher")?;
    keys(cx, window, &["escape"])?;
    // A drag in a shell selects; copy puts the text on the clipboard. The
    // first click focuses the pane.
    click(cx, window, at(20.0, 52.0))?;
    mouse(cx, window, down(at(20.0, 52.0)))?;
    mouse(cx, window, drag_to(at(300.0, 75.0)))?;
    mouse(cx, window, up(at(300.0, 75.0)))?;
    keys(cx, window, &[copy])?;
    shot(cx, window, &out, "10d-selected")?;
    let copied = cx.update(|cx| cx.read_from_clipboard().and_then(|item| item.text()));
    println!("copied: {copied:?}");

    // A Git project, opened from the CLI, shows in the sidebar.
    let repo = repository()?;
    let twyx = std::env::var_os("TWYX_BINARY").context("TWYX_BINARY names the twyx binary")?;
    let opened = std::process::Command::new(twyx)
        .arg("open")
        .arg(&repo)
        .status()
        .context("run twyx open")?;
    anyhow::ensure!(opened.success(), "twyx open {} failed", repo.display());
    pump(cx, window, Duration::from_secs(2))?;
    shot(cx, window, &out, "11-project")?;
    keys(cx, window, &["ctrl-g", "w", "j"])?;
    shot(cx, window, &out, "12-workspace-mode")?;
    keys(cx, window, &["n", "t", "o", "p", "i", "c"])?;
    shot(cx, window, &out, "13-new-worktree")?;
    keys(cx, window, &["enter"])?;
    pump(cx, window, Duration::from_secs(3))?;
    shot(cx, window, &out, "14-worktree-made")?;
    keys(cx, window, &["ctrl-g", "w", "x"])?;
    pump(cx, window, Duration::from_secs(1))?;
    shot(cx, window, &out, "15-remove")?;
    keys(cx, window, &["escape", "ctrl-g", "b"])?;
    shot(cx, window, &out, "16-undocked")?;
    keys(cx, window, &["ctrl-g", "b", "ctrl-g", "?"])?;
    shot(cx, window, &out, "17-keys")?;
    keys(cx, window, &["escape"])?;
    review(cx, window, &out)
}

/// A repository with changes since its base, opened and reviewed.
fn review(cx: &mut HeadlessAppContext, window: AnyWindowHandle, out: &Path) -> Result<()> {
    let repo = repository()?;
    std::fs::write(repo.join("README.md"), "scenes\nwith a second line\n")?;
    std::fs::create_dir_all(repo.join("src"))?;
    let body: String = (1..=30)
        .map(|n| format!("pub fn step_{n}(value: u32) -> u32 {{\n    value + {n}\n}}\n\n"))
        .collect();
    std::fs::write(repo.join("src/lib.rs"), &body)?;
    git(&repo, &["add", "src/lib.rs"])?;
    git(
        &repo,
        &[
            "-c",
            "user.name=twyx",
            "-c",
            "user.email=twyx@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "steps",
        ],
    )?;
    let edited = body
        .replace("value + 2\n", "value.saturating_add(2)\n")
        .replace(
            "pub fn step_15(value: u32) -> u32 {\n    value + 15\n}\n",
            "/// Fifteen on, saturating.\npub fn step_15(value: u32) -> u32 {\n    value.saturating_add(15)\n}\n",
        )
        .replace("pub fn step_28(value: u32) -> u32 {\n    value + 28\n}\n\n", "");
    std::fs::write(repo.join("src/lib.rs"), edited)?;
    let twyx = std::env::var_os("TWYX_BINARY").context("TWYX_BINARY names the twyx binary")?;
    let opened = std::process::Command::new(&twyx)
        .arg("open")
        .arg(&repo)
        .status()
        .context("run twyx open")?;
    anyhow::ensure!(opened.success(), "twyx open {} failed", repo.display());
    pump(cx, window, Duration::from_secs(2))?;
    keys(cx, window, &["ctrl-g", "v"])?;
    pump(cx, window, Duration::from_secs(2))?;
    shot(cx, window, out, "20-review")?;
    keys(cx, window, &["j"])?;
    shot(cx, window, out, "21-review-second-file")?;
    keys(cx, window, &["space"])?;
    pump(cx, window, Duration::from_secs(1))?;
    shot(cx, window, out, "22-review-marked")?;
    keys(cx, window, &["s", "n"])?;
    shot(cx, window, out, "23-review-sides")?;
    keys(cx, window, &["shift-c"])?;
    shot(cx, window, out, "24-review-commit")?;
    keys(cx, window, &["escape"])
}

/// A Git repository with one commit, in a new temporary directory.
fn repository() -> Result<std::path::PathBuf> {
    let repo = std::env::temp_dir().join(format!("twyx-scenes-{}", std::process::id()));
    std::fs::create_dir_all(&repo)?;
    std::fs::write(repo.join("README.md"), "scenes\n")?;
    for args in [
        &["init", "--quiet", "--initial-branch=main"][..],
        &["add", "README.md"],
        &[
            "-c",
            "user.name=twyx",
            "-c",
            "user.email=twyx@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "start",
        ],
    ] {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .status()
            .context("run git")?;
        anyhow::ensure!(status.success(), "git {args:?} failed");
    }
    Ok(repo)
}

fn git(repo: &Path, args: &[&str]) -> Result<()> {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .status()
        .context("run git")?;
    anyhow::ensure!(status.success(), "git {args:?} failed");
    Ok(())
}
