//! The Trash view: recently deleted items with a read-only preview and
//! one-click restore. Deleted items appear nowhere else in the app.

use crate::app::{AppState, Nox, relative_time};
use crate::theme::Theme;
use gpui::{
    AnyElement, App, ClickEvent, Context, ElementId, FontWeight, SharedString, Window, div,
    prelude::*, px,
};
use gpui_component::{
    Icon,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
};
use gpui_rsx::rsx;
use nox_core::{ItemId, ItemType};

/// Height of the "Recently deleted" heading block. The preview column is
/// offset by the same amount so its panel top lines up with the list table's,
/// rather than with the heading above it.
const LIST_HEADING_HEIGHT: f32 = 42.;

/// The design's primary Restore control: a light-filled pill with a dark
/// `rotate-ccw` glyph and label. Shared by the list rows and the preview
/// panel's action bar, which differ only in size.
fn restore_button(
    id: impl Into<ElementId>,
    theme: Theme,
    height: f32,
    font_size: f32,
    icon_size: f32,
    cx: &mut App,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    Button::new(id)
        .custom(
            ButtonCustomVariant::new(cx)
                .color(theme.inverse)
                .hover(theme.inverse_hover)
                .active(theme.inverse_press)
                .foreground(theme.on_inverse),
        )
        .h(px(height))
        .px(px(12.))
        .rounded(px(7.))
        .on_click(on_click)
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(7.))
                .child(
                    Icon::empty()
                        .path("icons/rotate-ccw.svg")
                        .size(px(icon_size))
                        .text_color(theme.on_inverse),
                )
                .child(
                    div()
                        .text_size(px(font_size))
                        .font_weight(FontWeight(650.))
                        .text_color(theme.on_inverse)
                        .child("Restore"),
                ),
        )
        .into_any_element()
}

/// The design's secondary action: surface fill with a visible outline, used
/// for the preview panel's Cancel.
fn cancel_button(
    id: impl Into<ElementId>,
    theme: Theme,
    cx: &mut App,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    Button::new(id)
        .custom(
            ButtonCustomVariant::new(cx)
                .color(theme.surface)
                .hover(theme.row_hover)
                .active(theme.raised)
                .foreground(theme.text_soft),
        )
        .h(px(36.))
        .px(px(12.))
        .rounded(px(7.))
        .border_1()
        .border_color(theme.border_strong)
        .on_click(on_click)
        .child(
            div()
                .text_size(px(11.))
                .font_weight(FontWeight(600.))
                .text_color(theme.text_soft)
                .child("Cancel"),
        )
        .into_any_element()
}

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
        let data_dir = self.data_dir.clone();
        let locker = cx.entity();

        let rows = deleted
            .into_iter()
            .map(|item| {
                let restore_locker = locker.clone();
                let select_locker = locker.clone();
                let item_id = item.item_id;
                let is_login = item.payload.item_type == ItemType::Login;
                // A deleted item keeps its fetched favicon until it is purged;
                // `resolved_item_icon` falls back to the type's default glyph
                // when there is no cached image.
                let row_icon = crate::icons::render_resolved_icon(
                    crate::icons::resolved_item_icon(
                        &data_dir,
                        &crate::icons::item_key(item_id),
                        &item.payload,
                        self.local_icon_selections
                            .get(&crate::icons::item_key(item_id)),
                    ),
                    17.,
                    theme.text_secondary,
                );
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
                            w={px(36.)} h={px(36.)} rounded={px(8.)} bg={theme.item_icon}
                        >
                            {row_icon}
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
                        {restore_button(
                            SharedString::from(format!("trash-restore-{item_id}")),
                            theme,
                            32.,
                            11.,
                            13.,
                            cx,
                            move |_, _window, app| {
                                restore_locker.update(app, |locker, cx| locker.restore_item(item_id, cx));
                            },
                        )}
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
                        <div flex flex_col gap={px(4.)} flex_shrink_0 h={px(LIST_HEADING_HEIGHT)}>
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
                    <div flex flex_col gap={px(12.)} w={px(392.)} flex_shrink_0>
                        // Spacer mirroring the list's heading block, so the
                        // preview panel starts level with the table itself.
                        <div h={px(LIST_HEADING_HEIGHT)} flex_shrink_0 />
                        {preview}
                    </div>
                </div>
            </div>
        }
        .into_any_element()
    }

    fn render_trash_preview(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let selected = self.session().and_then(|session| {
            let item_id = session.trash_selected?;
            session
                .list
                .deleted
                .iter()
                .find(|deleted| deleted.item_id == item_id)
                .cloned()
        });

        let Some(item) = selected else {
            return rsx! {
                <div
                    id="trash-preview-empty"
                    flex flex_col items_center justify_center gap={px(10.)}
                    w_full flex_1 min_h={px(0.)}
                    bg={theme.surface} rounded={px(10.)}
                    border_1 borderColor={theme.border}
                >
                    <Icon base={Icon::empty().path("icons/trash-2.svg").size(px(24.)).text_color(theme.text_ghost)} />
                    <div text_xs textColor={theme.text_muted}>"Select a deleted item to preview it"</div>
                </div>
            }
            .into_any_element();
        };

        let item_id = item.item_id;
        let is_login = item.payload.item_type == ItemType::Login;
        let heading = if is_login {
            "Deleted login"
        } else {
            "Deleted secure note"
        };
        let type_label = if is_login { "Login" } else { "Secure note" };
        let identity_icon = crate::icons::render_resolved_icon(
            crate::icons::resolved_item_icon(
                &self.data_dir,
                &crate::icons::item_key(item_id),
                &item.payload,
                self.local_icon_selections
                    .get(&crate::icons::item_key(item_id)),
            ),
            20.,
            theme.text_secondary,
        );
        let when = format!("Deleted {}", relative_time(item.deleted_at_ms));
        let title = item.payload.title.clone();

        // Read-only preview: the password is never revealed here, only in the
        // item editor once the item has actually been restored.
        let fields: Vec<(&'static str, &'static str, String)> = if is_login {
            vec![
                (
                    "WEBSITE",
                    "icons/globe.svg",
                    item.payload.uris.first().cloned().unwrap_or_default(),
                ),
                (
                    "USERNAME",
                    "icons/user-round.svg",
                    item.payload.username.clone(),
                ),
                ("PASSWORD", "icons/lock.svg", "••••••••••••••••".to_string()),
            ]
        } else {
            vec![(
                "NOTE",
                "icons/file-lock.svg",
                item.payload.notes.lines().next().unwrap_or("").to_string(),
            )]
        };

        let field_rows = fields
            .into_iter()
            .filter(|(label, _, value)| *label == "PASSWORD" || !value.is_empty())
            .map(|(label, icon, value)| {
                rsx! {
                    <div flex flex_col gap={px(7.)} w_full>
                        <div
                            fontFamily="Geist Mono" fontSize={px(9.)}
                            fontWeight={FontWeight::BOLD} textColor={theme.text_subtle}
                        >{label}</div>
                        <div
                            flex items_center gap={px(9.)} w_full
                            h={px(42.)} px={px(11.)} rounded={px(7.)}
                            bg={theme.field} border_1 borderColor={theme.field_border}
                        >
                            <Icon base={Icon::empty().path(icon).size(px(14.)).text_color(theme.icon_muted)} />
                            <div fontSize={px(12.)} textColor={theme.text_soft} truncate>{value}</div>
                        </div>
                    </div>
                }
            })
            .collect::<Vec<_>>();

        let close_locker = cx.entity();
        let cancel_locker = cx.entity();
        let restore_locker = cx.entity();

        rsx! {
            <div
                id="trash-preview"
                flex flex_col w_full flex_1 min_h={px(0.)}
                bg={theme.surface} rounded={px(10.)}
                border_1 borderColor={theme.border}
            >
                <div
                    flex items_center justify_between w_full
                    h={px(64.)} px={px(18.)} flex_shrink_0
                    border_b_1 borderColor={theme.border}
                >
                    <div flex flex_col gap={px(3.)}>
                        <div
                            fontFamily="Geist Mono" fontSize={px(9.)}
                            fontWeight={FontWeight::BOLD} textColor={theme.text_muted}
                        >"PREVIEW RESTORE"</div>
                        <div fontSize={px(15.)} fontWeight={FontWeight::SEMIBOLD} textColor={theme.text}>{heading}</div>
                    </div>
                    <Button
                        base={Button::new("trash-preview-close")
                            .ghost()
                            .icon(Icon::empty().path("icons/x.svg").size(px(15.)).text_color(theme.icon_muted))
                            .tooltip("Close preview")
                            .on_click(move |_, _window, app| {
                                close_locker.update(app, |locker, cx| locker.clear_trash_selection(cx));
                            })}
                    />
                </div>
                <div flex flex_col gap={px(22.)} flex_1 min_h={px(0.)} overflow_y_scroll py={px(22.)} px={px(20.)}>
                    <div flex items_center gap={px(12.)} w_full>
                        <div
                            flex items_center justify_center flex_shrink_0
                            w={px(44.)} h={px(44.)} rounded={px(10.)} bg={theme.item_icon}
                        >
                            {identity_icon}
                        </div>
                        <div flex flex_col gap={px(3.)} flex_1 min_w={px(0.)}>
                            <div fontSize={px(15.)} fontWeight={FontWeight::SEMIBOLD} textColor={theme.text} truncate>{title}</div>
                            <div fontSize={px(11.)} textColor={theme.text_muted}>{when}</div>
                        </div>
                    </div>
                    <div
                        flex items_center gap={px(8.)} w_full
                        h={px(40.)} px={px(11.)} rounded={px(8.)}
                        bg={theme.raised} border_1 borderColor={theme.border}
                    >
                        <Icon base={Icon::empty().path("icons/lock.svg").size(px(14.)).text_color(theme.icon_muted)} />
                        <div fontSize={px(11.)} fontWeight={FontWeight::MEDIUM} textColor={theme.text_secondary}>
                            "Read-only preview"
                        </div>
                    </div>
                    {div().flex().flex_col().gap(px(14.)).w_full().children(field_rows)}
                    <div flex flex_col gap={px(8.)} w_full>
                        <div flex items_center justify_between w_full>
                            <div fontSize={px(11.)} textColor={theme.text_muted}>"Item type"</div>
                            <div fontSize={px(11.)} fontWeight={FontWeight::MEDIUM} textColor={theme.text_soft}>{type_label}</div>
                        </div>
                        <div flex items_center justify_between w_full>
                            <div fontSize={px(11.)} textColor={theme.text_muted}>"Deleted"</div>
                            <div fontSize={px(11.)} fontWeight={FontWeight::MEDIUM} textColor={theme.text_soft}>
                                {relative_time(item.deleted_at_ms)}
                            </div>
                        </div>
                    </div>
                </div>
                <div
                    flex items_center justify_end gap={px(10.)} w_full
                    h={px(76.)} px={px(16.)} flex_shrink_0
                    border_t_1 borderColor={theme.border}
                >
                    {cancel_button("trash-preview-cancel", theme, cx, move |_, _window, app| {
                        cancel_locker.update(app, |locker, cx| locker.clear_trash_selection(cx));
                    })}
                    {restore_button(
                        "trash-preview-restore",
                        theme,
                        36.,
                        11.,
                        14.,
                        cx,
                        move |_, _window, app| {
                            restore_locker.update(app, |locker, cx| locker.restore_item(item_id, cx));
                        },
                    )}
                </div>
            </div>
        }
        .into_any_element()
    }
}
