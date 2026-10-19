//! The desktop client: one window, the focused pane, drawn on the GPU.

mod keys;
mod paint;
mod terminal;

use gpui::{App, AppContext, Bounds, WindowBounds, WindowOptions, px, size};

fn main() {
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
