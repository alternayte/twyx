//! The desktop client: one window, the focused pane, drawn on the GPU.
//! `--specimen [light|dark]` opens the theme specimen instead.

mod keys;
mod paint;
mod panes;
mod specimen;
mod terminal;

use gpui::{App, AppContext, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};
use twyx_core::theme::Variant;

fn main() {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() == Some("--specimen") {
        let paper = match args.next().as_deref() {
            None => None,
            Some("light") => Some(Variant::Light),
            Some("dark") => Some(Variant::Dark),
            Some(other) => {
                eprintln!("twyx-desktop: the specimen paper is light or dark, not {other:?}");
                std::process::exit(2);
            }
        };
        specimen(paper);
        return;
    }
    let keymap = match twyx_core::paths::Paths::discover()
        .map_err(|error| format!("{error:#}"))
        .and_then(|paths| {
            twyx_core::config::load(&paths.config.join("config.toml"))
                .map_err(|error| error.to_string())
        }) {
        Ok(config) => config.keymap,
        Err(error) => {
            eprintln!("twyx-desktop: {error}");
            std::process::exit(1);
        }
    };
    gpui_platform::application().run(move |cx: &mut App| {
        cx.set_reduce_motion(system_reduces_motion());
        let bounds = Bounds::centered(None, size(px(1100.0), px(700.0)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| terminal::Terminal::new(window, cx, keymap)),
        );
        match opened {
            Ok(_) => cx.activate(true),
            Err(error) => eprintln!("twyx-desktop: {error:#}"),
        }
    });
}

fn specimen(paper: Option<Variant>) {
    gpui_platform::application().run(move |cx: &mut App| {
        cx.set_reduce_motion(system_reduces_motion());
        let bounds = Bounds::centered(None, size(px(1280.0), px(800.0)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("twyx specimen".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| specimen::Specimen::new(window, cx, paper)),
        );
        match opened {
            Ok(_) => cx.activate(true),
            Err(error) => eprintln!("twyx-desktop: {error:#}"),
        }
    });
}

/// Whether the system asks for reduced motion. GPUI does not read it.
fn system_reduces_motion() -> bool {
    let (program, args, reduced): (&str, &[&str], &str) = if cfg!(target_os = "macos") {
        (
            "defaults",
            &["read", "com.apple.universalaccess", "reduceMotion"],
            "1",
        )
    } else {
        (
            "gsettings",
            &["get", "org.gnome.desktop.interface", "enable-animations"],
            "false",
        )
    };
    std::process::Command::new(program)
        .args(args)
        .output()
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).trim() == reduced)
}
