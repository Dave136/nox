mod app;

use gpui::{AppContext, WindowOptions};
use gpui_component::{Root, init};

fn main() {
    gpui::Application::new().run(|cx| {
        init(cx);
        match locker_core::default_vault_path() {
            Ok(path) => {
                cx.open_window(WindowOptions::default(), move |window, cx| {
                    let view = cx.new(|cx| {
                        app::Locker::new(path, app::DEFAULT_INACTIVITY_TIMEOUT, window, cx)
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
