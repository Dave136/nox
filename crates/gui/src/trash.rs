//! The Trash view: recently deleted items with a read-only preview and
//! one-click restore. Deleted items appear nowhere else in the app.

use crate::app::{AppState, Nox, relative_time};
use crate::theme::Theme;
use gpui::{AnyElement, Context, FontWeight, SharedString, Window, div, prelude::*, px};
use gpui_component::{Icon, Sizable, button::Button};
use gpui_rsx::rsx;
use nox_core::{ItemId, ItemType};

impl Nox {
    /// Restore a tombstoned item by appending a fresh upsert of its last known
    /// payload. Sync converges on its own: the new change carries a later HLC,
    /// so it wins the projection on every device.
    pub(crate) fn restore_item(&mut self, item_id: ItemId, cx: &mut Context<Self>) {
        let AppState::Unlocked(session) = &mut self.state else {
            return;
        };
        let Some(payload) = session
            .list
            .deleted
            .iter()
            .find(|deleted| deleted.item_id == item_id)
            .map(|deleted| deleted.payload.clone())
        else {
            return;
        };
        if session.vault.update_item(item_id, &payload).is_err() {
            return;
        }
        session.list.upsert(item_id, payload);
        if session.trash_selected == Some(item_id) {
            session.trash_selected = None;
        }
        self.refresh_deleted();
        cx.notify();
    }

    pub(crate) fn select_trash_item(&mut self, item_id: ItemId, cx: &mut Context<Self>) {
        if let Some(session) = self.session_mut() {
            session.trash_selected = Some(item_id);
        }
        cx.notify();
    }

    pub(crate) fn clear_trash_selection(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.session_mut() {
            session.trash_selected = None;
        }
        cx.notify();
    }

    pub(crate) fn render_trash(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::current(cx);
        let deleted = self
            .session()
            .map(|session| session.list.deleted.clone())
            .unwrap_or_default();
        let count = deleted.len();
        let locker = cx.entity();

        let rows = deleted
            .into_iter()
            .map(|item| {
                let restore_locker = locker.clone();
                let select_locker = locker.clone();
                let item_id = item.item_id;
                let is_login = item.payload.item_type == ItemType::Login;
                let type_icon = if is_login {
                    "icons/key-round.svg"
                } else {
                    "icons/file-lock.svg"
                };
                let type_label = if is_login { "Login" } else { "Secure note" };
                let subtitle = if is_login && !item.payload.username.is_empty() {
                    item.payload.username.clone()
                } else {
                    type_label.to_string()
                };
                let when = format!("Deleted {}", relative_time(item.deleted_at_ms));
                let title = item.payload.title.clone();

                rsx! {
                    <div
                        id={SharedString::from(format!("trash-row-{item_id}"))}
                        flex items_center gap={px(12.)}
                        h={px(76.)} px={px(16.)} w_full
                        border_b_1 borderColor={theme.border}
                        hover={|this| this.bg(theme.row_hover)}
                        onClick={move |_, _window, app| {
                            select_locker.update(app, |locker, cx| locker.select_trash_item(item_id, cx));
                        }}
                    >
                        <div
                            flex items_center justify_center flex_shrink_0
                            w={px(36.)} h={px(36.)} rounded={px(8.)} bg={theme.raised}
                        >
                            <Icon base={Icon::empty().path(type_icon).size(px(17.)).text_color(theme.item_icon)} />
                        </div>
                        <div flex flex_col gap={px(4.)} flex_1 min_w={px(0.)}>
                            <div flex items_center gap={px(8.)}>
                                <div text_sm fontWeight={FontWeight::SEMIBOLD} textColor={theme.text}>{title}</div>
                                <div
                                    flex items_center h={px(20.)} px={px(7.)}
                                    rounded={px(5.)} bg={theme.raised}
                                    fontFamily="Geist Mono" fontSize={px(9.)}
                                    fontWeight={FontWeight::SEMIBOLD} textColor={theme.text_secondary}
                                >{type_label}</div>
                            </div>
                            <div fontSize={px(11.)} textColor={theme.text_muted}>{subtitle}</div>
                        </div>
                        <div fontSize={px(11.)} textColor={theme.text_muted} flex_shrink_0>{when}</div>
                        <Button
                            base={Button::new(SharedString::from(format!("trash-restore-{item_id}")))
                                .icon(Icon::empty().path("icons/rotate-ccw.svg").size(px(13.)))
                                .label("Restore")
                                .small()
                                .on_click(move |_, _window, app| {
                                    restore_locker.update(app, |locker, cx| locker.restore_item(item_id, cx));
                                })}
                        />
                    </div>
                }
            })
            .collect::<Vec<_>>();

        let preview = self.render_trash_preview(cx);

        rsx! {
            <div id="trash-workspace" flex flex_col flex_1 min_w={px(0.)} h_full bg={theme.canvas}>
                <div
                    id="trash-header"
                    flex items_center justify_between w_full
                    h={px(88.)} px={px(32.)} flex_shrink_0
                    border_b_1 borderColor={theme.border}
                >
                    <div flex flex_col gap={px(3.)}>
                        <div text_xl fontWeight={FontWeight::SEMIBOLD} textColor={theme.text}>"Trash"</div>
                        <div text_xs textColor={theme.text_muted}>"Review and recover recently deleted vault items"</div>
                    </div>
                    <div
                        flex items_center gap={px(7.)}
                        h={px(30.)} px={px(10.)} rounded={px(7.)}
                        bg={theme.surface} border_1 borderColor={theme.border}
                    >
                        <Icon base={Icon::empty().path("icons/clock-3.svg").size(px(14.)).text_color(theme.icon_muted)} />
                        <div fontSize={px(11.)} fontWeight={FontWeight::MEDIUM} textColor={theme.text_secondary}>
                            "Recoverable until sync cleanup"
                        </div>
                    </div>
                </div>
                <div flex flex_1 min_h={px(0.)} gap={px(16.)} p={px(28.)} pt={px(20.)}>
                    <div flex flex_col gap={px(12.)} flex_1 min_w={px(0.)}>
                        <div flex flex_col gap={px(4.)} flex_shrink_0>
                            <div flex items_center gap={px(8.)}>
                                <div fontSize={px(16.)} fontWeight={FontWeight::SEMIBOLD} textColor={theme.text}>
                                    "Recently deleted"
                                </div>
                                <div
                                    flex items_center h={px(22.)} px={px(8.)}
                                    rounded={px(11.)} bg={theme.raised}
                                    fontFamily="Geist Mono" fontSize={px(10.)}
                                    fontWeight={FontWeight::BOLD} textColor={theme.text_count}
                                >{count.to_string()}</div>
                            </div>
                            <div text_xs textColor={theme.text_muted}>
                                "Items stay recoverable until sync cleanup."
                            </div>
                        </div>
                        <div
                            id="trash-list"
                            flex flex_col flex_1 min_h={px(0.)} overflow_y_scroll
                            bg={theme.surface} rounded={px(10.)}
                            border_1 borderColor={theme.border}
                        >
                            {if count == 0 {
                                rsx! {
                                    <div flex flex_col items_center justify_center gap={px(10.)} flex_1 py={px(48.)}>
                                        <Icon base={Icon::empty().path("icons/trash-2.svg").size(px(24.)).text_color(theme.text_ghost)} />
                                        <div text_xs textColor={theme.text_muted}>"Nothing in the trash"</div>
                                    </div>
                                }.into_any_element()
                            } else {
                                div().flex().flex_col().children(rows).into_any_element()
                            }}
                        </div>
                    </div>
                    {preview}
                </div>
            </div>
        }
        .into_any_element()
    }

    fn render_trash_preview(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        rsx! {
            <div
                w={px(392.)} h_full flex_shrink_0
                bg={theme.surface} rounded={px(10.)}
                border_1 borderColor={theme.border}
            />
        }
        .into_any_element()
    }
}
