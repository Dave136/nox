mod app;
mod assets;
mod backup;
mod change_password;
mod clipboard;
mod conflicts;
mod detail;
mod favicon;
mod icons;
mod item_editor;
mod locked;
mod nav;
mod settings;
mod theme;
mod trash;
mod ui;
mod vault_dialogs;
mod vault_list;
mod vaults;
mod workspace;

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
        // Identifies the window to the desktop environment (X11 WM_CLASS /
        // Wayland xdg_toplevel app_id). Without this, DEs can't match the
        // window to a .desktop entry and show a generic icon + "Unknown".
        // Must match the `.desktop` file's name (see packaging/nox.desktop).
        app_id: Some("nox".into()),
        window_min_size: Some(gpui::Size {
            width: gpui::px(920.),
            height: gpui::px(740.),
        }),
        ..Default::default()
    }
}

fn main() {
    let app = application()
        .with_assets(Assets)
        .with_http_client(std::sync::Arc::new(reqwest_client::ReqwestClient::new()));

    app.run(|cx| {
        // Must come before anything renders. GPUI matches a font family by
        // exact name and silently discards the requested weight and style when
        // no face carries that name, so until these are registered every
        // `font_weight(..)` and `italic()` in the app is a no-op.
        cx.text_system()
            .add_fonts(assets::fonts())
            .expect("register the embedded Geist faces");

        init(cx);
        theme::init(cx);
        match nox_core::default_data_dir() {
            Ok(data_dir) => {
                cx.open_window(window_options(), move |window, cx| {
                    let view = cx.new(|cx| {
                        app::Nox::new(
                            data_dir,
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
                .expect("open Nox window");
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
