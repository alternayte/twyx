//! The desktop client: one window, one pane, drawn on the GPU.

mod keys;
mod paint;
mod terminal;

use gpui::{App, AppContext, Bounds, WindowBounds, WindowOptions, px, size};

fn main() {
    gpui_platform::application().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1100.0), px(700.0)), cx);
        let opened = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| terminal::Terminal::new(window, cx)),
        );
        match opened {
            Ok(_) => cx.activate(true),
            Err(error) => eprintln!("twyx-desktop: {error:#}"),
        }
    });
}
