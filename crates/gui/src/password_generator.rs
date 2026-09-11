//! Standalone "Generate password" quick action on Home: reuses the
//! item-editor's [`GeneratorPopoverState`] and generation logic, but operates
//! on its own `Nox::password_generator` field instead of an open item editor,
//! so a password can be generated and copied without creating or editing an
//! item.
use crate::app::Nox;
use crate::clipboard::COPY_FEEDBACK_DURATION;
use crate::item_editor::{GeneratorPopoverState, input, toggle_generator_class};
use crate::theme::Theme;
use gpui::{AnyElement, Context, FontWeight, Window, div, prelude::*, px};
use gpui_component::{
    Disableable, Sizable,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    checkbox::Checkbox,
    input::Input,
    popover::Popover,
};
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

    /// Home's 5th quick-action tile: the tile itself is the `Popover`
    /// trigger, following `render_add_item_menu` in `workspace.rs`.
    pub(crate) fn render_password_generator_tile(
        &mut self,
        _hovered: Option<bool>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::current(cx);
        let muted_foreground = theme.text_muted;
        // `Popover::trigger` requires `Selectable`, which `AnyElement` (what
        // `home_quick_action` returns) does not implement, so this tile is
        // built as a plain `Button` matching the other tiles' icon-box/label
        // layout and resting colors instead of routing through
        // `home_quick_action`/`animated_auth_button` (see report deviation).
        let id = "home-generate-password";
        let icon_box = div()
            .size(px(32.))
            .rounded(px(8.))
            .bg(theme.field)
            .flex()
            .items_center()
            .justify_center()
            .group_hover(id, |style| style.bg(theme.surface))
            .child(
                gpui_component::Icon::empty()
                    .path("icons/wand-sparkles.svg")
                    .size(px(16.))
                    .text_color(theme.text_secondary),
            );
        let content = div()
            .flex()
            .items_center()
            .justify_between()
            .w_full()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(11.))
                    .child(icon_box)
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.text)
                            .child("Generate password"),
                    ),
            );
        let tile = Button::new(id)
            .group(id)
            .flex_1()
            .h(px(62.))
            .px(px(14.))
            .rounded(px(8.))
            .custom(
                ButtonCustomVariant::new(cx)
                    .color(theme.surface)
                    .hover(theme.raised)
                    .foreground(theme.text),
            )
            .bg(theme.surface)
            .child(content);
        let generator_open = self.password_generator.open;
        let generated = self
            .password_generator
            .generated
            .as_ref()
            .map(|password| String::from_utf8_lossy(password.as_bytes()).into_owned());
        let length_input = self.password_generator.length_input.clone();
        let classes = self.password_generator.classes;
        let copied = self.password_generator_copied;
        let locker = cx.entity();
        let locker_for_open = locker.clone();
        let locker_for_generate = locker.clone();
        let locker_for_copy = locker.clone();
        let class_locker = locker.clone();
        let class_checkbox = move |id: &'static str, label: &'static str, class: CharClasses| {
            Checkbox::new(id)
                .label(label)
                .checked(classes.contains(class))
                .on_click({
                    let locker = class_locker.clone();
                    move |checked, _, app| {
                        locker.update(app, |locker, cx| {
                            locker.set_standalone_generator_class(class, *checked, cx)
                        });
                    }
                })
        };
        Popover::new("home-generate-password")
            .trigger(tile)
            .open(generator_open)
            .on_open_change(move |open, window, app| {
                locker_for_open.update(app, |locker, cx| {
                    if *open {
                        locker.open_password_generator(window, cx);
                    } else {
                        locker.set_standalone_generator_open(false, cx);
                    }
                });
            })
            .content(move |_popover, _window, _cx| {
                let preview = generated.clone().unwrap_or_else(|| "Click Generate".into());
                let copy_label = if copied { "Copied!" } else { "Copy password" };
                div()
                    .id("password-generator-panel")
                    .p(px(16.))
                    .w(px(272.))
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .rounded(px(12.))
                    .border_1()
                    .border_color(theme.field_border)
                    .bg(theme.surface)
                    .text_color(theme.text)
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight(650.))
                            .child("Password generator"),
                    )
                    .child(Input::new(&length_input))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .font_weight(FontWeight::NORMAL)
                            .child(class_checkbox(
                                "generator-lower",
                                "Lowercase",
                                CharClasses::LOWER,
                            ))
                            .child(class_checkbox(
                                "generator-upper",
                                "Uppercase",
                                CharClasses::UPPER,
                            ))
                            .child(class_checkbox(
                                "generator-digits",
                                "Digits",
                                CharClasses::DIGITS,
                            ))
                            .child(class_checkbox(
                                "generator-symbols",
                                "Symbols",
                                CharClasses::SYMBOLS,
                            )),
                    )
                    .child(
                        div()
                            .font_weight(FontWeight::NORMAL)
                            .text_color(muted_foreground)
                            .truncate()
                            .child(preview),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(
                                Button::new("generate-standalone-password")
                                    .outline()
                                    .small()
                                    .label("Generate")
                                    .disabled(classes.is_empty())
                                    .on_click({
                                        let locker = locker_for_generate.clone();
                                        move |_, _, app| {
                                            locker.update(app, |locker, cx| {
                                                locker.generate_standalone_password(cx)
                                            });
                                        }
                                    }),
                            )
                            .child(
                                Button::new("copy-generated-password")
                                    .primary()
                                    .small()
                                    .label(copy_label)
                                    .on_click({
                                        let locker = locker_for_copy.clone();
                                        move |_, window, app| {
                                            locker.update(app, |locker, cx| {
                                                locker.copy_generated_password(window, cx)
                                            });
                                        }
                                    }),
                            ),
                    )
                    .into_any_element()
            })
            .into_any_element()
    }
}
