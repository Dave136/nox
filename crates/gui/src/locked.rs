use crate::app::{
    FormState, Nox, animated_auth_button, vault_dropdown_trailing, vault_row_content,
};
use crate::assets::logo;
use crate::backup;
use crate::theme::Theme;
use gpui::{
    BoxShadow, Context, FontWeight, KeyDownEvent, Window, div, point, prelude::*, px, rgba,
};
use gpui_component::{
    Disableable, Sizable,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    input::Input,
    popover::Popover,
    select::SelectEvent,
};
use gpui_rsx::rsx;

impl Nox {
    pub(crate) fn render_no_vault(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = Theme::current(cx);
        let pending = self.create_state == FormState::Pending;
        let backup_busy = !self.backup.is_idle();
        let error = match &self.create_state {
            FormState::Error(message) => div()
                .text_sm()
                .text_center()
                .text_color(theme.danger)
                .child(message.clone()),
            FormState::Idle | FormState::Pending => div(),
        };
        let backup_status = match &self.backup.operation {
            backup::BackupOperation::Failed(message) => div()
                .text_sm()
                .text_center()
                .text_color(theme.danger)
                .child(message.clone()),
            _ => div(),
        };
        let create_button = Button::new("create-vault-submit")
            .w_full()
            .h(px(44.))
            .rounded(px(7.))
            .disabled(pending || backup_busy)
            .loading(pending)
            .on_click(cx.listener(|this, _, window, cx| this.create_vault(window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .font_weight(FontWeight::BOLD)
                    .child(
                        gpui_component::Icon::empty()
                            .path("icons/shield-plus.svg")
                            .size(px(15.)),
                    )
                    .child(if pending {
                        "Creating…"
                    } else {
                        "Create vault"
                    }),
            )
            .when(pending || backup_busy, |button| {
                button
                    .bg(theme.inverse_disabled)
                    .text_color(theme.on_inverse)
            });
        let create_button = animated_auth_button(
            "create-vault-submit",
            create_button,
            self.auth_hovered.get("create-vault-submit").copied(),
            (
                theme.inverse,
                theme.inverse_hover,
                theme.inverse_active,
                theme.on_inverse,
            ),
            cx,
        );
        let restore_button = Button::new("create-restore-backup")
            .w_full()
            .h(px(32.))
            .px(px(12.))
            .disabled(backup_busy)
            .on_click(cx.listener(|this, _, window, cx| this.begin_restore(window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .w_full()
                    .gap(px(7.))
                    .child(
                        gpui_component::Icon::empty()
                            .path("icons/archive-restore.svg")
                            .size(px(14.))
                            .text_color(theme.text_subtle),
                    )
                    .child("Restore a backup instead"),
            );
        let restore_button = animated_auth_button(
            "create-restore-backup",
            restore_button,
            self.auth_hovered.get("create-restore-backup").copied(),
            (
                theme.canvas,
                theme.raised,
                theme.border,
                theme.text_secondary,
            ),
            cx,
        );
        rsx! {
            <div
                id="no-vault-view"
                size_full
                flex
                items_center
                justify_center
                bg={theme.canvas}
                p={px(36.)}
                onKeyDown={cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    match event.keystroke.key.as_str() {
                        "enter" => this.create_vault(window, cx),
                        "escape" => window.remove_window(),
                        _ => {}
                    }
                })}
            >
                <div
                    id="no-vault-card"
                    flex
                    flex_col
                    gap={px(15.)}
                    w={px(416.)}
                >
                    <div flex flex_col items_center gap={px(8.)}>
                        {logo(52., theme.text)}
                        <div flex flex_col items_center gap={px(4.)}>
                            <div text_lg fontWeight={FontWeight::SEMIBOLD} textColor={theme.text}>
                                {"Create your vault"}
                            </div>
                            <div text_xs text_center textColor={theme.text_muted}>
                                {"Choose a master password to secure your data"}
                            </div>
                        </div>
                    </div>
                    <div
                        id="create-recovery-warning"
                        flex
                        items_center
                        gap={px(10.)}
                        h={px(48.)}
                        px={px(12.)}
                        rounded={px(8.)}
                        bg={theme.surface}
                        border_1
                        borderColor={theme.border}
                    >
                        <div size={px(28.)} flex items_center justify_center rounded_full bg={theme.raised}>
                            <div text_sm fontWeight={FontWeight::BOLD} textColor={theme.text}>{"!"}</div>
                        </div>
                        <div flex flex_col flex_1 gap={px(2.)}>
                            <div text_sm fontWeight={FontWeight::SEMIBOLD} textColor={theme.text_soft}>
                                {"No password recovery"}
                            </div>
                            <div text_xs textColor={theme.text_subtle}>
                                {"Store your master password somewhere safe"}
                            </div>
                        </div>
                        {gpui_component::Icon::empty()
                            .path("icons/shield-alert.svg")
                            .size(px(14.))
                            .text_color(theme.text_subtle)}
                    </div>
                    <div id="create-vault-name" flex flex_col gap={px(7.)}>
                        <div text_xs fontWeight={FontWeight::BOLD} textColor={theme.text_subtle}>
                            {"VAULT NAME"}
                        </div>
                            <Input
                                base={Input::new(&self.create_name)
                                    .prefix(
                                    gpui_component::Icon::empty()
                                        .path("icons/database.svg")
                                        .size(px(15.))
                                        .text_color(theme.icon_muted),
                                    )
                                    .aria_label("Vault name")}
                                h={px(44.)}
                                bg={theme.surface}
                                borderColor={theme.border_strong}
                                rounded={px(8.)}
                        />
                    </div>
                    <div id="create-vault-password" flex flex_col gap={px(7.)}>
                        <div text_xs fontWeight={FontWeight::BOLD} textColor={theme.text_subtle}>
                            {"MASTER PASSWORD"}
                        </div>
                        <Input
                            base={Input::new(&self.create_password)
                                .mask_toggle()
                                .prefix(
                                    gpui_component::Icon::empty()
                                        .path("icons/lock.svg")
                                        .size(px(15.))
                                        .text_color(theme.icon_muted),
                                )
                                .aria_label("Master password")}
                            h={px(44.)}
                            bg={theme.surface}
                            borderColor={theme.border_strong}
                            rounded={px(8.)}
                        />
                    </div>
                    <div id="create-vault-confirm" flex flex_col gap={px(7.)}>
                        <div text_xs fontWeight={FontWeight::BOLD} textColor={theme.text_subtle}>
                            {"CONFIRM MASTER PASSWORD"}
                        </div>
                        <Input
                            base={Input::new(&self.create_confirm)
                                .mask_toggle()
                                .prefix(
                                    gpui_component::Icon::empty()
                                        .path("icons/lock.svg")
                                        .size(px(15.))
                                        .text_color(theme.icon_muted),
                                )
                                .aria_label("Confirm master password")}
                            h={px(44.)}
                            bg={theme.surface}
                            borderColor={theme.border_strong}
                            rounded={px(8.)}
                        />
                    </div>
                    {error}
                    {create_button}
                    <div flex flex_col gap={px(8.)}>
                        <div h={px(1.)} w_full bg={theme.border} />
                        {if self.vaults.vaults.is_empty() {
                            div().into_any_element()
                        } else {
                            Button::new("create-back-to-vault-list")
                                .h(px(32.))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.return_to_unlock(window, cx);
                                }))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(7.))
                                        .child("Back"),
                                )
                                .into_any_element()
                        }}
                        {restore_button}
                        {backup_status}
                        <div text_xs text_center textColor={theme.text_ghost}>
                            {"Enter to create · Esc to close"}
                        </div>
                    </div>
                    <div flex items_center justify_center gap={px(7.)} textColor={theme.text_subtle}>
                        {gpui_component::Icon::empty().path("icons/shield-check.svg").size(px(13.))}
                        <div text_xs>{"Encrypted locally · You hold the keys"}</div>
                    </div>
                </div>
            </div>
        }
    }

    pub(crate) fn render_locked(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = Theme::current(cx);
        let pending = self.unlock_state == FormState::Pending;
        let backup_busy = !self.backup.is_idle();
        let trigger_content = self.active_vault.as_ref().map_or_else(
            || div().child("Select a vault").into_any_element(),
            |vault| {
                vault_row_content(
                    theme,
                    vault,
                    Some(
                        gpui_component::Icon::empty()
                            .path("icons/chevron-down.svg")
                            .size(px(14.))
                            .text_color(theme.icon_muted)
                            .into_any_element(),
                    ),
                )
            },
        );
        let trigger_variant = ButtonCustomVariant::new(cx)
            .color(theme.surface)
            .hover(theme.surface)
            .active(theme.surface)
            .foreground(theme.text_soft);
        let trigger = Button::new("vault-select-trigger")
            .custom(trigger_variant)
            .w_full()
            .h(px(48.))
            .px(px(12.))
            .rounded(px(8.))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface)
            .child(trigger_content);
        let vaults = self.vaults.vaults.clone();
        let selected_id = self.active_vault.as_ref().map(|vault| vault.id.clone());
        let select = self.vault_select.clone();
        let vault_select = Popover::new("vault-select-popover")
            .appearance(false)
            .trigger(trigger)
            .content(move |_state, _window, cx| {
                let popover = cx.entity();
                let rows = vaults.iter().cloned().enumerate().map(|(index, vault)| {
                    let missing = !vault.path.is_file();
                    let checked = selected_id.as_ref() == Some(&vault.id);
                    let (icon, color) = vault_dropdown_trailing(theme, missing, checked);
                    let trailing = gpui_component::Icon::empty()
                        .path(icon)
                        .size(px(14.))
                        .text_color(color)
                        .into_any_element();
                    let select = select.clone();
                    let popover = popover.clone();
                    let id = vault.id.clone();
                    div()
                        .id(("vault-option", index))
                        .w_full()
                        .h(px(40.))
                        .px(px(8.))
                        .rounded(px(6.))
                        .bg(if checked { theme.raised } else { theme.surface })
                        .when(!checked && !missing, |row| {
                            row.hover(|style| style.bg(theme.raised))
                        })
                        .when(!missing, |row| {
                            row.cursor_pointer().on_click(move |_, window, app| {
                                select.update(app, |_, cx| {
                                    cx.emit(SelectEvent::Confirm(Some(id.clone())));
                                });
                                popover.update(app, |state, cx| state.dismiss(window, cx));
                            })
                        })
                        .child(vault_row_content(theme, &vault, Some(trailing)))
                });
                div()
                    .w(px(416.))
                    .p(px(4.))
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.surface)
                    .shadow(vec![BoxShadow {
                        color: rgba(0x00000066).into(),
                        offset: point(px(0.), px(4.)),
                        blur_radius: px(12.),
                        spread_radius: px(0.),
                        inset: false,
                    }])
                    .children(rows)
            });
        let error = match &self.unlock_state {
            FormState::Error(message) => div()
                .text_sm()
                .text_color(theme.danger)
                .child(message.clone()),
            FormState::Idle | FormState::Pending => div(),
        };
        let backup_status = match &self.backup.operation {
            backup::BackupOperation::Failed(message) => div()
                .text_sm()
                .text_center()
                .text_color(theme.danger)
                .child(message.clone()),
            backup::BackupOperation::Succeeded(message) => div()
                .text_sm()
                .text_center()
                .text_color(theme.text_subtle)
                .child(message.clone()),
            backup::BackupOperation::AwaitingRestoreConfirmation { archive_path } => div()
                .text_sm()
                .text_center()
                .text_color(theme.text_subtle)
                .child(format!("Restore: {}", archive_path.display())),
            _ => div(),
        };
        let unlock_button = Button::new("unlock-submit")
            .w_full()
            .h(px(44.))
            .rounded(px(7.))
            .disabled(pending || backup_busy)
            .loading(pending)
            .on_click(cx.listener(|this, _, window, cx| this.unlock_vault(window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(12.))
                    .font_weight(FontWeight::BOLD)
                    .child(
                        gpui_component::Icon::empty()
                            .path("icons/lock-keyhole-open.svg")
                            .size(px(15.))
                            .text_color(theme.on_inverse),
                    )
                    .child(if pending {
                        "Unlocking…"
                    } else {
                        "Unlock vault"
                    }),
            )
            .when(pending || backup_busy, |button| {
                button
                    .bg(theme.inverse_disabled)
                    .text_color(theme.on_inverse)
            });
        let unlock_button = animated_auth_button(
            "unlock-submit",
            unlock_button,
            self.auth_hovered.get("unlock-submit").copied(),
            (
                theme.inverse,
                theme.inverse_hover,
                theme.inverse_active,
                theme.on_inverse,
            ),
            cx,
        );
        let create_button = Button::new("locked-create-vault")
            .w_full()
            .h(px(32.))
            .px(px(12.))
            .on_click(cx.listener(|this, _, window, cx| this.begin_create_from_picker(window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .w_full()
                    .gap(px(7.))
                    .child(
                        gpui_component::Icon::empty()
                            .path("icons/plus.svg")
                            .size(px(14.))
                            .text_color(theme.text_subtle),
                    )
                    .child("Create a new vault"),
            );
        let create_button = animated_auth_button(
            "locked-create-vault",
            create_button,
            self.auth_hovered.get("locked-create-vault").copied(),
            (
                theme.canvas,
                theme.raised,
                theme.border,
                theme.text_secondary,
            ),
            cx,
        );
        let restore_button = Button::new("restore-backup")
            .w_full()
            .h(px(32.))
            .px(px(12.))
            .disabled(backup_busy)
            .on_click(cx.listener(|this, _, window, cx| this.begin_restore(window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .w_full()
                    .gap(px(7.))
                    .child(
                        gpui_component::Icon::empty()
                            .path("icons/archive-restore.svg")
                            .size(px(14.))
                            .text_color(theme.text_subtle),
                    )
                    .child("Restore a backup instead"),
            );
        let restore_button = animated_auth_button(
            "restore-backup",
            restore_button,
            self.auth_hovered.get("restore-backup").copied(),
            (
                theme.canvas,
                theme.raised,
                theme.border,
                theme.text_secondary,
            ),
            cx,
        );
        rsx! {
            <div
                id="locked-view"
                size_full
                flex
                items_center
                justify_center
                bg={theme.canvas}
                p={px(36.)}
                onKeyDown={cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    match event.keystroke.key.as_str() {
                        "enter" => this.unlock_vault(window, cx),
                        "escape" => window.remove_window(),
                        _ => {}
                    }
                })}
            >
                <div
                    id="locked-card"
                    flex
                    flex_col
                    gap={px(18.)}
                    w={px(416.)}
                >
                    <div flex flex_col items_center gap={px(8.)}>
                        {logo(52., theme.text)}
                        <div flex flex_col items_center gap={px(4.)}>
                            <div text_lg fontWeight={FontWeight::SEMIBOLD} textColor={theme.text}>
                                {"Unlock your vault"}
                            </div>
                            <div text_xs text_center textColor={theme.text_muted}>
                                {"Enter your master password to continue"}
                            </div>
                        </div>
                    </div>
                    <div id="locked-vault-summary">
                        {vault_select}
                    </div>
                    <div id="unlock-password" flex flex_col gap={px(7.)}>
                        <div text_xs fontWeight={FontWeight::BOLD} textColor={theme.text_subtle}>
                            {"MASTER PASSWORD"}
                        </div>
                        <Input
                            base={Input::new(&self.unlock_password)
                                .large()
                                .focus_bordered(false)
                                .text_size(px(11.))
                                .gap(px(10.))
                                .mask_toggle()
                                .prefix(
                                    gpui_component::Icon::empty()
                                        .path("icons/lock.svg")
                                        .size(px(15.))
                                        .text_color(theme.icon_muted),
                                )
                                .aria_label("Master password")}
                            h={px(44.)}
                            bg={theme.surface}
                            borderColor={theme.border_strong}
                            rounded={px(8.)}
                        />
                    </div>
                    {error}
                    {unlock_button}
                    <div flex flex_col gap={px(8.)}>
                        <div h={px(1.)} w_full bg={theme.border} />
                        {create_button}
                        {restore_button}
                        {backup_status}
                        <div text_xs text_center textColor={theme.text_ghost}>
                            {"Enter to unlock · Esc to close"}
                        </div>
                    </div>
                    <div flex items_center justify_center gap={px(7.)} textColor={theme.text_subtle}>
                        {gpui_component::Icon::empty()
                            .path("icons/shield-check.svg")
                            .size(px(13.))}
                        <div text_xs>{"Encrypted locally · Works offline"}</div>
                    </div>
                </div>
            </div>
        }
    }
}
