mod app;
mod assets;

use gpui::{AppContext, WindowDecorations, WindowOptions};
use gpui_platform::application;
// use gpui_component_assets::Assets;
use gpui_component::{Root, init};

use crate::assets::Assets;

/// The app draws its own title bar (`ui::window::controls::WindowControls`),
/// so the platform's own titlebar/decorations would just double up as a
/// stray border around our content — ask for client-side decorations and no
/// native titlebar instead.
fn window_options() -> WindowOptions {
    WindowOptions {
        titlebar: None,
        window_decorations: Some(WindowDecorations::Client),
        ..Default::default()
    }
}

fn main() {
    let app = application().with_assets(Assets);

    app.run(|cx| {
        init(cx);
        match locker_core::default_vault_path() {
            Ok(path) => {
                cx.open_window(window_options(), move |window, cx| {
                    let view = cx.new(|cx| {
                        app::Locker::new(
                            path,
                            app::DEFAULT_INACTIVITY_TIMEOUT,
                            app::DEFAULT_CLIPBOARD_TIMEOUT,
                            window,
                            cx,
                        )
                    });
                    // `Root`'s own CSD border wrapper is themed (light/dark) and would
                    // flip color with `Theme::change` (e.g. white once Home switches to
                    // light mode) around our fully custom title bar — we draw all window
                    // chrome ourselves, so disable it.
                    cx.new(|cx| Root::new(view, window, cx).bordered(false))
                })
                .expect("open Locker window");
            }
            Err(error) => {
                cx.open_window(window_options(), move |window, cx| {
                    let view = cx.new(|_| app::FatalStartupError::new(error));
                    cx.new(|cx| Root::new(view, window, cx).bordered(false))
                })
                .expect("open fatal-error window");
            }
        }
    });
}
