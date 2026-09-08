//! The Trash view: recently deleted items with a read-only preview and
//! one-click restore. Deleted items appear nowhere else in the app.

use crate::app::Nox;
use crate::theme::Theme;
use gpui::{AnyElement, Context, Window, div, prelude::*, px};
use gpui_rsx::rsx;

impl Nox {
    pub(crate) fn render_trash(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::current(cx);
        rsx! {
            <div id="trash-workspace" flex flex_col flex_1 min_w={px(0.)} h_full bg={theme.canvas} />
        }
        .into_any_element()
    }
}
