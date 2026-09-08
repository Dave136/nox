use crate::app::{Nox, RemoveVaultDialogState, RenameVaultDialogState};
use crate::theme::APP_FONT_FAMILY;
use gpui::{
    AnyElement, Context, FontWeight, KeyDownEvent, MouseButton, SharedString, Window, div,
    prelude::*, px, relative, rgb, rgba,
};
use gpui_component::FocusTrapElement as _;
use gpui_component::input::Input;

impl Nox {
    /// Open the in-app rename dialog for the vault at `index`.
    pub(crate) fn begin_rename_vault(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.vaults.vaults.get(index) else {
            return;
        };
        let original: SharedString = entry.name.clone().into();
        let input = Self::new_input(window, cx, "Vault name", false);
        input.update(cx, |input, cx| {
            input.set_value(original.to_string(), window, cx);
        });
        self.rename_vault_prior_focus = window.focused(cx);
        self.rename_vault_dialog = Some(RenameVaultDialogState {
            index,
            original,
            input: input.clone(),
            error: false,
        });
        // The name is what the user came to change, so the field takes focus.
        Self::focus_input(&input, window, cx);
        cx.notify();
    }

    pub(crate) fn cancel_rename_vault(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.rename_vault_dialog = None;
        if let Some(focus) = self.rename_vault_prior_focus.take() {
            focus.focus(window, cx);
        }
        cx.notify();
    }

    pub(crate) fn confirm_rename_vault(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.rename_vault_dialog.clone() else {
            return;
        };
        let name = dialog.input.read(cx).value().to_string();
        if self.rename_vault(dialog.index, name, window, cx) {
            self.rename_vault_dialog = None;
            self.rename_vault_prior_focus = None;
            cx.notify();
        } else if let Some(state) = self.rename_vault_dialog.as_mut() {
            state.error = true;
            Self::focus_input(&dialog.input, window, cx);
            cx.notify();
        }
    }

    /// Open the in-app confirmation for removing the vault at `index`.
    pub(crate) fn begin_remove_vault(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.vaults.vaults.get(index) else {
            return;
        };
        self.remove_vault_prior_focus = window.focused(cx);
        self.remove_vault_dialog = Some(RemoveVaultDialogState {
            index,
            name: entry.name.clone().into(),
            file_exists: entry.path.is_file(),
            // Removing only unregisters the vault until the user deliberately
            // opts into the destructive path.
            delete_files: false,
        });
        self.remove_vault_cancel_focus.focus(window, cx);
        cx.notify();
    }

    pub(crate) fn toggle_remove_vault_files(&mut self, cx: &mut Context<Self>) {
        if let Some(dialog) = &mut self.remove_vault_dialog
            && dialog.file_exists
        {
            dialog.delete_files = !dialog.delete_files;
            cx.notify();
        }
    }

    pub(crate) fn cancel_remove_vault(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.remove_vault_dialog = None;
        if let Some(focus) = self.remove_vault_prior_focus.take() {
            focus.focus(window, cx);
        }
        cx.notify();
    }

    pub(crate) fn confirm_remove_vault(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.remove_vault_dialog.take() else {
            return;
        };
        self.remove_vault_prior_focus = None;
        self.remove_vault(dialog.index, dialog.delete_files, window, cx);
    }

    pub(crate) fn render_rename_vault_dialog(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let dialog = self.rename_vault_dialog.clone()?;
        let cancel_focus = self.rename_vault_cancel_focus.clone();
        let confirm_focus = self.rename_vault_confirm_focus.clone();
        let dialog_focus = self.rename_vault_dialog_focus.clone();
        let cancel_for_a11y = cx.entity();
        let confirm_for_a11y = cx.entity();

        let cancel = div()
            .id("rename-vault-cancel")
            .debug_selector(|| "rename-vault-cancel".to_owned())
            .w(px(69.))
            .h(px(34.))
            .px(px(16.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(7.))
            .border_1()
            .border_color(rgb(0x3A4450))
            .bg(rgb(0x222731))
            .font_family(APP_FONT_FAMILY)
            .text_size(px(11.))
            .font_weight(FontWeight(550.))
            .text_color(rgb(0xDDE3E8))
            .cursor_pointer()
            .track_focus(&cancel_focus)
            .role(gpui::Role::Button)
            .aria_label("Cancel")
            .focus_visible(|style| style.border_color(rgb(0x77B8DF)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    window.prevent_default();
                    this.cancel_rename_vault(window, cx);
                }
            }))
            .on_click(cx.listener(|this, _, window, cx| this.cancel_rename_vault(window, cx)))
            .on_a11y_action(gpui::AccessibleAction::Click, move |_, window, app| {
                cancel_for_a11y.update(app, |this, cx| this.cancel_rename_vault(window, cx));
            })
            .child("Cancel");

        let confirm = div()
            .id("rename-vault-confirm")
            .debug_selector(|| "rename-vault-confirm".to_owned())
            .w(px(64.))
            .h(px(34.))
            .px(px(16.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(7.))
            .border_1()
            .border_color(rgb(0xE3E6ED))
            .bg(rgb(0xE3E6ED))
            .font_family(APP_FONT_FAMILY)
            .text_size(px(11.))
            .font_weight(FontWeight(650.))
            .text_color(rgb(0x1A1D22))
            .cursor_pointer()
            .track_focus(&confirm_focus)
            .role(gpui::Role::Button)
            .aria_label("Save")
            .focus_visible(|style| style.border_color(rgb(0x77B8DF)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    window.prevent_default();
                    this.confirm_rename_vault(window, cx);
                }
            }))
            .on_click(cx.listener(|this, _, window, cx| this.confirm_rename_vault(window, cx)))
            .on_a11y_action(gpui::AccessibleAction::Click, move |_, window, app| {
                confirm_for_a11y.update(app, |this, cx| this.confirm_rename_vault(window, cx));
            })
            .child("Save");

        let error = dialog.error.then(|| {
            div()
                .debug_selector(|| "rename-vault-error".to_owned())
                .font_family(APP_FONT_FAMILY)
                .text_size(px(10.))
                .line_height(relative(1.45))
                .text_color(rgb(0xC9959A))
                .child("A vault needs a name.")
        });

        Some(
            div()
                .id("rename-vault-layer")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x0A0C0F99))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.cancel_rename_vault(window, cx)),
                )
                .child(
                    div()
                        .id("rename-vault-dialog")
                        .debug_selector(|| "rename-vault-dialog".to_owned())
                        .w(px(440.))
                        .flex()
                        .flex_col()
                        .gap(px(16.))
                        .p(px(21.))
                        .rounded(px(12.))
                        .border_1()
                        .border_color(rgb(0x3A4450))
                        .bg(rgb(0x202630))
                        .track_focus(&dialog_focus)
                        .role(gpui::Role::Dialog)
                        .aria_label(format!("Rename {}", dialog.original))
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            match event.keystroke.key.as_str() {
                                "escape" => {
                                    window.prevent_default();
                                    this.cancel_rename_vault(window, cx);
                                }
                                // Enter submits from the field, the way every
                                // other form in the app behaves.
                                "enter" => {
                                    window.prevent_default();
                                    this.confirm_rename_vault(window, cx);
                                }
                                _ => {}
                            }
                        }))
                        .on_mouse_down(MouseButton::Left, |_, _, app| app.stop_propagation())
                        .child(
                            div()
                                .debug_selector(|| "rename-vault-title".to_owned())
                                .font_family(APP_FONT_FAMILY)
                                .text_size(px(15.))
                                .line_height(relative(1.2))
                                .font_weight(FontWeight(650.))
                                .text_color(rgb(0xF3F5F7))
                                .child("Rename vault"),
                        )
                        .child(
                            div()
                                .debug_selector(|| "rename-vault-body".to_owned())
                                .w_full()
                                .font_family(APP_FONT_FAMILY)
                                .text_size(px(11.))
                                .line_height(relative(1.45))
                                .text_color(rgb(0xB9C0C8))
                                .child(
                                    "Only the label changes. The vault file stays where it is, \
                                     so its folder keeps its original name.",
                                ),
                        )
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .flex_col()
                                .gap(px(7.))
                                .child(
                                    div()
                                        .font_family(APP_FONT_FAMILY)
                                        .text_size(px(9.))
                                        .font_weight(FontWeight(700.))
                                        .text_color(rgb(0x7F8996))
                                        .child("VAULT NAME"),
                                )
                                .child(
                                    Input::new(&dialog.input)
                                        .prefix(
                                            gpui_component::Icon::empty()
                                                .path("icons/database.svg")
                                                .size(px(14.))
                                                .text_color(rgb(0x737E8D)),
                                        )
                                        .aria_label("Vault name"),
                                )
                                .children(error),
                        )
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap(px(8.))
                                .child(cancel)
                                .child(confirm),
                        )
                        .focus_trap("rename-vault-focus-trap", &dialog_focus),
                )
                .into_any_element(),
        )
    }

    pub(crate) fn render_remove_vault_dialog(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let dialog = self.remove_vault_dialog.clone()?;
        let checked = dialog.delete_files;
        let warning = if checked {
            "Every password in this vault is destroyed. Nox has no password recovery and this \
             cannot be undone."
        } else {
            "Leave this off to keep the file — useful for a vault on a drive you unplug."
        };
        let option_fill = if checked { 0x2A2024 } else { 0x1B2029 };
        let option_border = if checked { 0xA9787D } else { 0x343D48 };
        let option_label = if checked { 0xE9C3C6 } else { 0xDDE3E8 };
        let option_warning = if checked { 0xC9959A } else { 0x7F8996 };
        let confirm_fill = if checked { 0xA9787D } else { 0xE3E6ED };
        let confirm_label = if checked { "Delete vault" } else { "Remove" };

        let option_focus = self.remove_vault_option_focus.clone();
        let cancel_focus = self.remove_vault_cancel_focus.clone();
        let confirm_focus = self.remove_vault_confirm_focus.clone();
        let dialog_focus = self.remove_vault_dialog_focus.clone();
        let toggle_for_a11y = cx.entity();
        let cancel_for_a11y = cx.entity();
        let confirm_for_a11y = cx.entity();

        let delete_option = dialog.file_exists.then(|| {
            div()
                .id("remove-vault-delete-files")
                .debug_selector(|| "remove-vault-delete-files".to_owned())
                .w_full()
                .flex()
                .items_start()
                .gap(px(10.))
                .p(px(11.))
                .rounded(px(8.))
                .border_1()
                .border_color(rgb(option_border))
                .bg(rgb(option_fill))
                .cursor_pointer()
                .track_focus(&option_focus)
                .role(gpui::Role::CheckBox)
                .aria_label("Also delete the vault file permanently")
                .aria_description(warning)
                .aria_toggled(if checked {
                    gpui::Toggled::True
                } else {
                    gpui::Toggled::False
                })
                .focus_visible(|style| style.border_color(rgb(0x77B8DF)))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        window.prevent_default();
                        this.toggle_remove_vault_files(cx);
                    }
                }))
                .on_click(cx.listener(|this, _, _, cx| this.toggle_remove_vault_files(cx)))
                .on_a11y_action(gpui::AccessibleAction::Click, move |_, _, app| {
                    toggle_for_a11y.update(app, |this, cx| {
                        this.toggle_remove_vault_files(cx);
                    });
                })
                .child(
                    div()
                        .flex_shrink_0()
                        .size(px(16.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .border_1()
                        .border_color(rgb(if checked { 0xA9787D } else { 0x3A4450 }))
                        .bg(rgb(if checked { 0xA9787D } else { 0x202630 }))
                        .when(checked, |checkbox| {
                            checkbox.child(
                                gpui_component::Icon::empty()
                                    .path("icons/check.svg")
                                    .size(px(11.))
                                    .text_color(rgb(0x12161C)),
                            )
                        }),
                )
                .child(
                    div()
                        .min_w(px(0.))
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap(px(5.))
                        .child(
                            div()
                                .debug_selector(|| "remove-vault-option-label".to_owned())
                                .font_family(APP_FONT_FAMILY)
                                .text_size(px(11.))
                                .line_height(relative(1.18))
                                .font_weight(FontWeight(550.))
                                .text_color(rgb(option_label))
                                .child("Also delete the vault file permanently"),
                        )
                        .child(
                            div()
                                .debug_selector(|| "remove-vault-option-warning".to_owned())
                                .font_family(APP_FONT_FAMILY)
                                .text_size(px(10.))
                                .line_height(px(15.))
                                .when(!checked, |text| text.whitespace_nowrap())
                                .text_color(rgb(option_warning))
                                .child(warning),
                        ),
                )
        });

        let cancel = div()
            .id("remove-vault-cancel")
            .debug_selector(|| "remove-vault-cancel".to_owned())
            .w(px(69.))
            .h(px(34.))
            .px(px(16.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(7.))
            .border_1()
            .border_color(rgb(0x3A4450))
            .bg(rgb(0x222731))
            .font_family(APP_FONT_FAMILY)
            .text_size(px(11.))
            .font_weight(FontWeight(550.))
            .text_color(rgb(0xDDE3E8))
            .cursor_pointer()
            .track_focus(&cancel_focus)
            .role(gpui::Role::Button)
            .aria_label("Cancel")
            .focus_visible(|style| style.border_color(rgb(0x77B8DF)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    window.prevent_default();
                    this.cancel_remove_vault(window, cx);
                }
            }))
            .on_click(cx.listener(|this, _, window, cx| {
                this.cancel_remove_vault(window, cx);
            }))
            .on_a11y_action(gpui::AccessibleAction::Click, move |_, window, app| {
                cancel_for_a11y.update(app, |this, cx| {
                    this.cancel_remove_vault(window, cx);
                });
            })
            .child("Cancel");

        let confirm = div()
            .id("remove-vault-confirm")
            .debug_selector(|| "remove-vault-confirm".to_owned())
            .w(px(if checked { 94. } else { 74. }))
            .h(px(34.))
            .px(px(16.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(7.))
            .border_1()
            .border_color(rgb(confirm_fill))
            .bg(rgb(confirm_fill))
            .font_family(APP_FONT_FAMILY)
            .text_size(px(11.))
            .font_weight(FontWeight(650.))
            .text_color(rgb(0x1A1D22))
            .cursor_pointer()
            .track_focus(&confirm_focus)
            .role(gpui::Role::Button)
            .aria_label(confirm_label)
            .focus_visible(|style| style.border_color(rgb(0x77B8DF)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    window.prevent_default();
                    this.confirm_remove_vault(window, cx);
                }
            }))
            .on_click(cx.listener(|this, _, window, cx| {
                this.confirm_remove_vault(window, cx);
            }))
            .on_a11y_action(gpui::AccessibleAction::Click, move |_, window, app| {
                confirm_for_a11y.update(app, |this, cx| {
                    this.confirm_remove_vault(window, cx);
                });
            })
            .child(confirm_label);

        Some(
            div()
                .id("remove-vault-modal-layer")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x0A0C0F99))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        this.cancel_remove_vault(window, cx);
                    }),
                )
                .child(
                    div()
                        .id("remove-vault-dialog")
                        .debug_selector(|| "remove-vault-dialog".to_owned())
                        .w(px(440.))
                        .flex()
                        .flex_col()
                        .gap(px(16.))
                        .p(px(21.))
                        .rounded(px(12.))
                        .border_1()
                        .border_color(rgb(0x3A4450))
                        .bg(rgb(0x202630))
                        .track_focus(&dialog_focus)
                        .role(gpui::Role::Dialog)
                        .aria_label(format!("Remove {} from the list?", dialog.name))
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            if event.keystroke.key.as_str() == "escape" {
                                window.prevent_default();
                                this.cancel_remove_vault(window, cx);
                            }
                        }))
                        .on_mouse_down(MouseButton::Left, |_, _, app| app.stop_propagation())
                        .child(
                            div()
                                .debug_selector(|| "remove-vault-title".to_owned())
                                .font_family(APP_FONT_FAMILY)
                                .text_size(px(15.))
                                .line_height(relative(1.2))
                                .font_weight(FontWeight(650.))
                                .text_color(rgb(0xF3F5F7))
                                .child(format!("Remove “{}” from the list?", dialog.name)),
                        )
                        .child(
                            div()
                                .debug_selector(|| "remove-vault-body".to_owned())
                                .w_full()
                                .font_family(APP_FONT_FAMILY)
                                .text_size(px(11.))
                                .line_height(relative(1.45))
                                .text_color(rgb(0xB9C0C8))
                                .child(
                                    "Nox stops listing this vault. Its file stays on disk, so you \
                                     can open it again later.",
                                ),
                        )
                        .children(delete_option)
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap(px(8.))
                                .child(cancel)
                                .child(confirm),
                        )
                        .focus_trap("remove-vault-focus-trap", &dialog_focus),
                )
                .into_any_element(),
        )
    }
}
