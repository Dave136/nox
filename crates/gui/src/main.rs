mod app;
mod assets;

use gpui::{AppContext, WindowOptions};
use gpui_platform::application;
// use gpui_component_assets::Assets;
use gpui_component::{Root, init};

use crate::assets::Assets;

fn main() {
    let app = application().with_assets(Assets);

    app.run(|cx| {
        init(cx);
        match locker_core::default_vault_path() {
            Ok(path) => {
                cx.open_window(WindowOptions::default(), move |window, cx| {
                    let view = cx.new(|cx| {
                        app::Locker::new(
                            path,
                            app::DEFAULT_INACTIVITY_TIMEOUT,
                            app::DEFAULT_CLIPBOARD_TIMEOUT,
                            window,
                            cx,
                        )
                    });
                    cx.new(|cx| Root::new(view, window, cx))
                })
                .expect("open Locker window");
            }
            Err(error) => {
                cx.open_window(WindowOptions::default(), move |window, cx| {
                    let view = cx.new(|_| app::FatalStartupError::new(error));
                    cx.new(|cx| Root::new(view, window, cx))
                })
                .expect("open fatal-error window");
            }
        }
    });
}
