#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod scope;

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{AppContext as _, TitlebarOptions, WindowBounds, WindowOptions, px, size};

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            Theme::change(ThemeMode::Dark, None, cx);

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(960.), px(920.)), cx)),
                window_min_size: Some(size(px(760.), px(480.))),
                titlebar: Some(TitlebarOptions {
                    title: Some("Bunker Morse".into()),
                    ..Default::default()
                }),
                ..Default::default()
            };
            gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| app::BunkerMorse::new(window, cx))
            })
            .expect("opening the main window");
            cx.activate(true);
        });
}
