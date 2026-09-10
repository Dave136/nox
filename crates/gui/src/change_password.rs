use crate::app::{AppState, ChangePasswordDialogState, FormState, Nox};
use crate::theme::{APP_FONT_FAMILY, Theme};
use gpui::{
    AnyElement, Context, Entity, FontWeight, KeyDownEvent, MouseButton, Window, div, prelude::*,
    px, relative, rgba,
};
use gpui_component::FocusTrapElement as _;
use gpui_component::input::{Input, InputState};
use nox_core::{SecretBytes, Vault, VaultError};
use std::path::Path;

const CHANGE_PASSWORD_ERROR_MESSAGE: &str = "Incorrect password.";
const CHANGE_PASSWORD_MIN_LENGTH: usize = 8;

/// Backdrop dim, matching `settings::dialog`'s `MODAL_BACKDROP` — not part of
/// [`Theme`]'s semantic roles, just this overlay's own scrim.
const MODAL_BACKDROP: u32 = 0x0A0C0F99;

impl Nox {
    /// Open the in-app "change vault password" dialog.
    pub(crate) fn begin_change_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.state, AppState::Unlocked(_)) {
            return;
        }
        let current = Self::new_input(window, cx, "Current password", true);
        let new_password = Self::new_input(window, cx, "New password", true);
        let confirm = Self::new_input(window, cx, "Confirm new password", true);
        self.change_password_prior_focus = window.focused(cx);
        self.change_password_state = FormState::Idle;
        self.change_password_dialog = Some(ChangePasswordDialogState {
            current: current.clone(),
            new_password,
            confirm,
        });
        Self::focus_input(&current, window, cx);
        cx.notify();
    }

    pub(crate) fn cancel_change_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.change_password_state == FormState::Pending {
            return;
        }
        self.change_password_dialog = None;
        self.change_password_state = FormState::Idle;
        if let Some(focus) = self.change_password_prior_focus.take() {
            focus.focus(window, cx);
        }
        cx.notify();
    }

    fn reset_change_password_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = Self::new_input(window, cx, "Current password", true);
        let new_password = Self::new_input(window, cx, "New password", true);
        let confirm = Self::new_input(window, cx, "Confirm new password", true);
        self.change_password_dialog = Some(ChangePasswordDialogState {
            current,
            new_password,
            confirm,
        });
    }

    pub(crate) fn submit_change_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.change_password_state == FormState::Pending {
            return;
        }
        let Some(dialog) = self.change_password_dialog.clone() else {
            return;
        };
        let Some(path) = self.vault_path().map(Path::to_path_buf) else {
            return;
        };

        let current = dialog.current.read(cx).value().to_string();
        let new_password = dialog.new_password.read(cx).value().to_string();
        let confirm = dialog.confirm.read(cx).value().to_string();
        // Replace the entities even on validation errors so failed submissions
        // cannot remain in InputState's undo history.
        self.reset_change_password_inputs(window, cx);

        if current.is_empty() {
            self.change_password_state = FormState::Error("Enter your current password.".into());
            self.focus_change_password_field(true, window, cx);
            cx.notify();
            return;
        }
        if new_password.is_empty() {
            self.change_password_state = FormState::Error("Enter a new password.".into());
            self.focus_change_password_field(false, window, cx);
            cx.notify();
            return;
        }
        if new_password != confirm {
            self.change_password_state = FormState::Error("Passwords do not match.".into());
            self.focus_change_password_field(false, window, cx);
            cx.notify();
            return;
        }
        if new_password.len() < CHANGE_PASSWORD_MIN_LENGTH {
            self.change_password_state = FormState::Error(
                format!("Password must be at least {CHANGE_PASSWORD_MIN_LENGTH} characters.")
                    .into(),
            );
            self.focus_change_password_field(false, window, cx);
            cx.notify();
            return;
        }

        self.change_password_state = FormState::Pending;
        cx.notify();
        let current_secret = SecretBytes::new(current.as_bytes());
        let new_secret = SecretBytes::new(new_password.as_bytes());
        self._change_password_task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    // `unlock` is the actual "current password" check — it
                    // must succeed before a `Vault` (and thus its verified
                    // `dek`) exists to re-wrap. `change_password` trusts that
                    // and does not re-derive the current password's key.
                    let vault = Vault::unlock(current_secret.as_bytes(), &path)?;
                    vault.change_password(new_secret.as_bytes())
                })
                .await;
            if let Some(this) = this.upgrade() {
                let _ = cx.update(|window, app| {
                    this.update(app, |this, cx| match result {
                        Ok(()) => {
                            this.change_password_dialog = None;
                            this.change_password_state = FormState::Idle;
                            if let Some(focus) = this.change_password_prior_focus.take() {
                                focus.focus(window, cx);
                            }
                            cx.notify();
                        }
                        Err(VaultError::IncorrectPasswordOrCorruptVault) => {
                            this.change_password_state =
                                FormState::Error(CHANGE_PASSWORD_ERROR_MESSAGE.into());
                            this.focus_change_password_field(true, window, cx);
                            cx.notify();
                        }
                        Err(error) => {
                            eprintln!("change password failed: {error}");
                            this.change_password_state = FormState::Error(
                                "Could not change the password. Try again.".into(),
                            );
                            this.focus_change_password_field(true, window, cx);
                            cx.notify();
                        }
                    });
                });
            }
        });
    }

    fn focus_change_password_field(
        &mut self,
        current_field: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self.change_password_dialog.clone() else {
            return;
        };
        if current_field {
            Self::focus_input(&dialog.current, window, cx);
        } else {
            Self::focus_input(&dialog.new_password, window, cx);
        }
    }

    pub(crate) fn render_change_password_dialog(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let dialog = self.change_password_dialog.clone()?;
        let theme = Theme::current(cx);
        let cancel_focus = self.change_password_cancel_focus.clone();
        let confirm_focus = self.change_password_confirm_focus.clone();
        let dialog_focus = self.change_password_dialog_focus.clone();
        let cancel_for_a11y = cx.entity();
        let confirm_for_a11y = cx.entity();
        let pending = self.change_password_state == FormState::Pending;

        let cancel = div()
            .id("change-password-cancel")
            .debug_selector(|| "change-password-cancel".to_owned())
            .w(px(69.))
            .h(px(34.))
            .px(px(16.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(7.))
            .border_1()
            .border_color(theme.field_border)
            .bg(theme.raised)
            .font_family(APP_FONT_FAMILY)
            .text_size(px(11.))
            .font_weight(FontWeight(550.))
            .text_color(theme.text_soft)
            .cursor_pointer()
            .track_focus(&cancel_focus)
            .role(gpui::Role::Button)
            .aria_label("Cancel")
            .focus_visible(|style| style.border_color(theme.accent))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    window.prevent_default();
                    this.cancel_change_password(window, cx);
                }
            }))
            .on_click(cx.listener(|this, _, window, cx| this.cancel_change_password(window, cx)))
            .on_a11y_action(gpui::AccessibleAction::Click, move |_, window, app| {
                cancel_for_a11y.update(app, |this, cx| this.cancel_change_password(window, cx));
            })
            .child("Cancel");

        let confirm = div()
            .id("change-password-confirm")
            .debug_selector(|| "change-password-confirm".to_owned())
            .w(px(64.))
            .h(px(34.))
            .px(px(16.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(7.))
            .border_1()
            .border_color(theme.inverse)
            .bg(theme.inverse)
            .font_family(APP_FONT_FAMILY)
            .text_size(px(11.))
            .font_weight(FontWeight(650.))
            .text_color(theme.on_inverse)
            .cursor_pointer()
            .track_focus(&confirm_focus)
            .role(gpui::Role::Button)
            .aria_label("Save")
            .focus_visible(|style| style.border_color(theme.accent))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    window.prevent_default();
                    this.submit_change_password(window, cx);
                }
            }))
            .on_click(cx.listener(|this, _, window, cx| this.submit_change_password(window, cx)))
            .on_a11y_action(gpui::AccessibleAction::Click, move |_, window, app| {
                confirm_for_a11y.update(app, |this, cx| this.submit_change_password(window, cx));
            })
            .child(if pending { "Saving…" } else { "Save" });

        let error = match &self.change_password_state {
            FormState::Error(message) => Some(
                div()
                    .debug_selector(|| "change-password-error".to_owned())
                    .font_family(APP_FONT_FAMILY)
                    .text_size(px(10.))
                    .line_height(relative(1.45))
                    .text_color(theme.danger)
                    .child(message.clone()),
            ),
            _ => None,
        };

        Some(
            div()
                .id("change-password-layer")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(MODAL_BACKDROP))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.cancel_change_password(window, cx)),
                )
                .child(
                    div()
                        .id("change-password-dialog")
                        .debug_selector(|| "change-password-dialog".to_owned())
                        .w(px(440.))
                        .flex()
                        .flex_col()
                        .gap(px(16.))
                        .p(px(21.))
                        .rounded(px(12.))
                        .border_1()
                        .border_color(theme.field_border)
                        .bg(theme.surface)
                        .track_focus(&dialog_focus)
                        .role(gpui::Role::Dialog)
                        .aria_label("Change vault password")
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            match event.keystroke.key.as_str() {
                                "escape" => {
                                    window.prevent_default();
                                    this.cancel_change_password(window, cx);
                                }
                                // Enter submits from a field, the way every
                                // other form in the app behaves.
                                "enter" => {
                                    window.prevent_default();
                                    this.submit_change_password(window, cx);
                                }
                                _ => {}
                            }
                        }))
                        .on_mouse_down(MouseButton::Left, |_, _, app| app.stop_propagation())
                        .child(
                            div()
                                .debug_selector(|| "change-password-title".to_owned())
                                .font_family(APP_FONT_FAMILY)
                                .text_size(px(15.))
                                .line_height(relative(1.2))
                                .font_weight(FontWeight(650.))
                                .text_color(theme.text)
                                .child("Change vault password"),
                        )
                        .child(
                            div()
                                .debug_selector(|| "change-password-body".to_owned())
                                .w_full()
                                .font_family(APP_FONT_FAMILY)
                                .text_size(px(11.))
                                .line_height(relative(1.45))
                                .text_color(theme.text_muted)
                                .child(
                                    "Only the password used to unlock this vault changes. Your \
                                     items stay exactly as they are.",
                                ),
                        )
                        .child(password_field(
                            theme,
                            "CURRENT PASSWORD",
                            "Current password",
                            &dialog.current,
                        ))
                        .child(password_field(
                            theme,
                            "NEW PASSWORD",
                            "New password",
                            &dialog.new_password,
                        ))
                        .child(password_field(
                            theme,
                            "CONFIRM NEW PASSWORD",
                            "Confirm new password",
                            &dialog.confirm,
                        ))
                        .children(error)
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
                        .focus_trap("change-password-focus-trap", &dialog_focus),
                )
                .into_any_element(),
        )
    }
}

fn password_field(
    theme: Theme,
    label: &'static str,
    aria_label: &'static str,
    input: &Entity<InputState>,
) -> AnyElement {
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
                .text_color(theme.text_subtle)
                .child(label),
        )
        .child(
            Input::new(input)
                .prefix(
                    gpui_component::Icon::empty()
                        .path("icons/lock.svg")
                        .size(px(14.))
                        .text_color(theme.icon_muted),
                )
                .mask_toggle()
                .aria_label(aria_label),
        )
        .into_any_element()
}
