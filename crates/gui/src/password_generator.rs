//! Title-bar password generator: reuses the item-editor's
//! [`GeneratorPopoverState`] and generation logic, but operates on its own
//! `Nox::password_generator` field instead of an open item editor, so a
//! password can be generated and copied from the title bar in any unlocked
//! view without creating or editing an item.
use crate::app::Nox;
use crate::clipboard::COPY_FEEDBACK_DURATION;
use crate::item_editor::{GeneratorPopoverState, input, toggle_generator_class};
use crate::theme::Theme;
use gpui::{AnyElement, Context, Window, div, prelude::*, px};
use nox_core::{CharClasses, MAX_LENGTH, SecretBytes, generate_password};

impl Nox {
    /// A fresh generator popover state, with the same defaults the item
    /// editor's own generator starts with.
    pub(crate) fn fresh_password_generator(
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> GeneratorPopoverState {
        let length_input = input("20", window, cx, "Length", false);
        GeneratorPopoverState {
            open: false,
            length: 20,
            classes: CharClasses::ALL,
            generated: None,
            length_input,
        }
    }

    pub(crate) fn open_password_generator(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.password_generator = Self::fresh_password_generator(window, cx);
        self.password_generator.open = true;
        cx.notify();
    }

    pub(crate) fn generate_standalone_password(&mut self, cx: &mut Context<Self>) {
        if let Ok(length) = self
            .password_generator
            .length_input
            .read(cx)
            .value()
            .parse::<usize>()
        {
            self.password_generator.length = length.clamp(1, MAX_LENGTH);
        }
        self.password_generator.generated = generate_password(
            self.password_generator.length,
            self.password_generator.classes,
        )
        .ok();
        cx.notify();
    }

    pub(crate) fn set_standalone_generator_class(
        &mut self,
        class: CharClasses,
        checked: bool,
        cx: &mut Context<Self>,
    ) {
        self.password_generator.classes =
            toggle_generator_class(self.password_generator.classes, class, checked);
        cx.notify();
    }

    pub(crate) fn copy_generated_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(password) = self.password_generator.generated.as_ref() else {
            return;
        };
        self.copy_secret(SecretBytes::new(password.as_bytes()), window, cx);
        self.password_generator_copied = true;
        self.password_generator_copy_epoch = self.password_generator_copy_epoch.wrapping_add(1);
        let epoch = self.password_generator_copy_epoch;
        cx.notify();
        self._password_generator_copy_task = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(COPY_FEEDBACK_DURATION).await;
            if let Some(this) = this.upgrade() {
                let _ = cx.update(|_, app| {
                    this.update(app, |this, cx| {
                        if this.password_generator_copy_epoch == epoch {
                            this.password_generator_copied = false;
                            cx.notify();
                        }
                    });
                });
            }
        });
    }

    pub(crate) fn set_standalone_generator_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.password_generator.open = open;
        cx.notify();
    }

    /// The title-bar generator's floating panel: an invisible full-screen
    /// backdrop (click-outside-to-dismiss, same technique
    /// `render_change_password_dialog` uses) with the actual panel
    /// corner-anchored near the title bar instead of centered, since this
    /// is a lightweight generate-and-copy tool, not a form.
    pub(crate) fn render_password_generator_overlay(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.password_generator.open {
            return None;
        }
        let theme = Theme::current(cx);
        let length_input = self.password_generator.length_input.clone();
        let classes = self.password_generator.classes;
        let generated = self
            .password_generator
            .generated
            .as_ref()
            .map(|password| String::from_utf8_lossy(password.as_bytes()).into_owned());
        let copied = self.password_generator_copied;
        let locker = cx.entity();
        let toggle_locker = locker.clone();
        let generate_locker = locker.clone();
        let copy_locker = locker.clone();
        let dismiss_locker = locker.clone();

        let panel = crate::item_editor::render_password_generator_content(
            theme,
            length_input,
            classes,
            generated,
            move |class, checked, _window, app| {
                toggle_locker.update(app, |locker, cx| {
                    locker.set_standalone_generator_class(class, checked, cx)
                });
            },
            move |_window, app| {
                generate_locker.update(app, |locker, cx| locker.generate_standalone_password(cx));
            },
            "pw-generator-copy",
            if copied { "Copied!" } else { "Copy password" },
            false,
            move |window, app| {
                copy_locker.update(app, |locker, cx| locker.copy_generated_password(window, cx));
            },
        );

        Some(
            div()
                .id("password-generator-overlay-layer")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .on_mouse_down(gpui::MouseButton::Left, move |_, _window, app| {
                    dismiss_locker.update(app, |locker, cx| {
                        locker.set_standalone_generator_open(false, cx)
                    });
                })
                .child(
                    div()
                        .absolute()
                        // Sits just under the title bar, aligned toward the
                        // right where the title-bar controls (including the
                        // trigger button) live.
                        .top(px(44.))
                        .right(px(12.))
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, app| app.stop_propagation())
                        .child(panel),
                )
                .into_any_element(),
        )
    }
}
