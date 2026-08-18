use gpui::{Context, Render, Window, WindowOptions, div, prelude::*};
use gpui_component::{Root, init};

struct Scaffold;

impl Render for Scaffold {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child("Locker")
    }
}

fn main() {
    gpui::Application::new().run(|cx| {
        init(cx);
        cx.open_window(WindowOptions::default(), |window, cx| {
            let view = cx.new(|_| Scaffold);
            cx.new(|cx| Root::new(view, window, cx))
        })
        .expect("open Locker window");
    });
}
