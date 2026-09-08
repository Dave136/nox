use crate::app::{
    Nox, SECURE_NOTE_PAPER, animated_auth_button, home_quick_action, home_stat_tile,
    recent_item_subtitle, relative_time, sync_status_pill,
};
use crate::nav::ActiveView;
use crate::theme::Theme;
use gpui::{
    AnyElement, Context, FontWeight, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    SharedString, Window, div, prelude::*, px,
};
use gpui_component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_component::popover::Popover;
use gpui_rsx::rsx;

/// Left-aligned icon + label for one "+ Add item" menu row.
///
/// A `Button`'s built-in `.label()` content wrapper always centers its
/// children (gpui-component hardcodes `justify_center` there); handing it
/// this `w_full` row instead — same trick as `vault_row_content` — leaves no
/// spare width for that centering to act on, so the icon and text land at
/// the start, and the text matches the trigger's own 14px label size.
fn add_item_row(icon_path: &'static str, label: &'static str, theme: Theme) -> AnyElement {
    div()
        .w_full()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(
            gpui_component::Icon::empty()
                .path(icon_path)
                .size(px(14.))
                .text_color(theme.text_secondary),
        )
        .child(div().text_size(px(14.)).text_color(theme.text).child(label))
        .into_any_element()
}

impl Nox {
    /// The primary "+ Add item" header button, shared by every workspace
    /// header (Home, All items, Logins, Secure notes): matches the Pencil
    /// "Add Item Button" node exactly (`#E3E6ED` fill, dark icon/label) and
    /// reuses the auth screens' animated hover.
    pub(crate) fn render_add_item_button(
        &mut self,
        id: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::current(cx);
        let button = Button::new(id)
            .h(px(38.))
            .px(px(16.))
            .rounded(px(8.))
            .on_click(cx.listener(|this, _, window, cx| this.open_create_editor(window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        gpui_component::Icon::empty()
                            .path("icons/plus.svg")
                            .size(px(16.))
                            .text_color(theme.canvas),
                    )
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight(500.))
                            .text_color(theme.canvas)
                            .child(
                                if self.session().map(|session| session.active_view)
                                    == Some(ActiveView::SecureNotes)
                                {
                                    "Add note"
                                } else {
                                    "Add item"
                                },
                            ),
                    ),
            );
        animated_auth_button(
            id,
            button,
            self.auth_hovered.get(id).copied(),
            (
                theme.inverse,
                theme.inverse_hover,
                theme.inverse_press,
                theme.on_inverse,
            ),
            cx,
        )
    }

    /// Home's "+ Add item" trigger: a dropdown for the two item types,
    /// instead of always creating a Login (Home has no active item-type
    /// filter to guess from, unlike the Logins/Secure notes views).
    pub(crate) fn render_add_item_menu(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let trigger = Button::new("home-add-item")
            .h(px(38.))
            .px(px(16.))
            .rounded(px(8.))
            .custom(
                ButtonCustomVariant::new(cx)
                    .color(theme.inverse)
                    .hover(theme.inverse_hover)
                    .active(theme.inverse_press)
                    .foreground(theme.on_inverse),
            )
            .bg(theme.inverse)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        gpui_component::Icon::empty()
                            .path("icons/plus.svg")
                            .size(px(16.))
                            .text_color(theme.canvas),
                    )
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight(500.))
                            .text_color(theme.canvas)
                            .child("Add item"),
                    ),
            );
        let locker = cx.entity();
        Popover::new("home-add-item-menu")
            .appearance(false)
            .trigger(trigger)
            .content(move |_state, _window, cx| {
                let popover = cx.entity();
                let login_locker = locker.clone();
                let login_popover = popover.clone();
                let note_locker = locker.clone();
                let note_popover = popover.clone();
                div()
                    .w(px(168.))
                    .p(px(4.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.surface)
                    .child(
                        Button::new("add-login")
                            .ghost()
                            .w_full()
                            .h(px(36.))
                            .px(px(10.))
                            .child(add_item_row("icons/key-square.svg", "Add Login", theme))
                            .on_click(move |_, window, app| {
                                login_locker.update(app, |locker, cx| {
                                    locker.open_create_editor_as(
                                        nox_core::ItemType::Login,
                                        window,
                                        cx,
                                    );
                                });
                                login_popover.update(app, |state, cx| state.dismiss(window, cx));
                            }),
                    )
                    .child(
                        Button::new("add-note")
                            .ghost()
                            .w_full()
                            .h(px(36.))
                            .px(px(10.))
                            .child(add_item_row("icons/file-lock.svg", "Add Note", theme))
                            .on_click(move |_, window, app| {
                                note_locker.update(app, |locker, cx| {
                                    locker.open_create_editor_as(
                                        nox_core::ItemType::SecureNote,
                                        window,
                                        cx,
                                    );
                                });
                                note_popover.update(app, |state, cx| state.dismiss(window, cx));
                            }),
                    )
            })
            .into_any_element()
    }

    pub(crate) fn render_unlocked(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = Theme::current(cx);
        let active_view = self
            .session()
            .map_or(ActiveView::AllItems, |session| session.active_view);
        // Checked before the Home early-return: the "+ Add item" menu opens
        // a create editor while `active_view` is still `Home`, and that
        // editor must win over Home's own empty-state layout.
        if self.uses_secure_note_workspace() {
            let nav = self.render_sidebar_nav(cx);
            let workspace = self.render_secure_note_workspace(window, cx);
            return rsx! {
                <div id="secure-note-workspace-shell" size_full flex bg={SECURE_NOTE_PAPER}>
                    {nav}
                    {workspace}
                </div>
            };
        }
        if self.uses_login_workspace() {
            let nav = self.render_sidebar_nav(cx);
            let workspace = self.render_login_workspace(window, cx);
            return rsx! {
                <div id="login-workspace-shell" size_full flex bg={theme.canvas}>
                    {nav}
                    {workspace}
                </div>
            };
        }
        if active_view == ActiveView::Home {
            let nav = self.render_sidebar_nav(cx);
            let home = self.render_home(window, cx);
            return rsx! { <div id="home-shell" size_full flex bg={theme.canvas}>{nav}{home}</div> };
        }
        if self
            .session()
            .is_some_and(|session| session.item_editor.is_some())
        {
            let locker = cx.entity();
            let content = self.render_item_editor(locker, window, cx);
            *self.item_editor_sheet_cell.borrow_mut() = Some(content);
        }
        let nav = self.render_sidebar_nav(cx);
        let add = self.render_add_item_button("header-add-item", cx);
        let list_toolbar = self.render_vault_list_toolbar(cx);
        let list = self.render_vault_list(window, cx);
        let detail = self.render_item_detail(window, cx);
        let conflict_panel = if self.conflicts_open {
            div()
                .p(px(20.))
                .pb(px(0.))
                .child(self.render_conflicts(window, cx))
                .into_any_element()
        } else {
            div().into_any_element()
        };
        let (total, logins, notes, favorites_count) = self
            .session()
            .map(|session| &session.list)
            .map_or((0, 0, 0, 0), |list| {
                let logins = list
                    .items
                    .iter()
                    .filter(|(_, item)| item.item_type == nox_core::ItemType::Login)
                    .count();
                let notes = list
                    .items
                    .iter()
                    .filter(|(_, item)| item.item_type == nox_core::ItemType::SecureNote)
                    .count();
                let favorites_count = list.items.iter().filter(|(_, item)| item.favorite).count();
                (list.items.len(), logins, notes, favorites_count)
            });
        let (page_title, item_count, item_noun) = match active_view {
            ActiveView::Home => ("Home", total, "items"),
            ActiveView::AllItems => ("All items", total, "items"),
            ActiveView::Logins => ("Logins", logins, "logins"),
            ActiveView::SecureNotes => ("Secure Notes", notes, "encrypted notes"),
            ActiveView::Favorites => ("Favorites", favorites_count, "items saved for quick access"),
        };
        rsx! {
            <div
                id="unlocked-view"
                size_full
                flex
                bg={theme.canvas}
                onMouseMove={cx.listener(|this, _: &MouseMoveEvent, _, cx| {
                    this.note_activity(cx);
                })}
                onMouseDown={(MouseButton::Left, cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.note_activity(cx);
                }))}
                onKeyDown={cx.listener(|this, _: &KeyDownEvent, _, cx| {
                    this.note_activity(cx);
                })}
            >
                {nav}
                <div flex flex_col flex_1 min_w={px(0.)} h_full>
                    <div
                        id="content-toolbar"
                        flex
                        items_center
                        justify_between
                        w_full
                        h={px(88.)}
                        px={px(32.)}
                        flex_shrink_0
                        border_b_1
                        borderColor={theme.border}
                    >
                        <div flex flex_col gap={px(3.)}>
                            <div text_xl fontWeight={FontWeight::SEMIBOLD} textColor={theme.text}>{page_title}</div>
                            <div text_xs textColor={theme.text_muted}>
                                {if matches!(active_view, ActiveView::SecureNotes | ActiveView::Favorites) {
                                    format!("{item_count} {item_noun}")
                                } else {
                                    format!("{item_count} {item_noun} in your vault")
                                }}
                            </div>
                        </div>
                        <div flex items_center gap={px(10.)}>
                            <Button
                                base={Button::new("lock-vault")
                                    .ghost()
                                    .icon(gpui_component::Icon::empty().path("icons/lock-keyhole-open.svg").text_color(theme.text_muted))
                                    .tooltip("Lock vault")
                                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                                        if !event.keystroke.modifiers.modified()
                                            && matches!(event.keystroke.key.as_str(), "enter" | "space")
                                        {
                                            window.prevent_default();
                                            this.lock_vault(window, cx);
                                        }
                                    }))
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.lock_vault(window, cx)),
                                    )}
                            />
                            {sync_status_pill(theme)}
                            {add}
                        </div>
                    </div>
                    {conflict_panel}
                    <div flex flex_col flex_1 min_h={px(0.)} p={px(28.)} pt={px(20.)} gap={px(16.)} bg={theme.canvas}>
                        {list_toolbar}
                        <div flex flex_1 min_h={px(0.)} gap={px(16.)}>
                            {list}
                            {detail}
                        </div>
                    </div>
                </div>
            </div>
        }
    }

    pub(crate) fn render_home(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::current(cx);
        let (total, logins, notes, recent) =
            self.session()
                .map(|session| &session.list)
                .map_or((0, 0, 0, Vec::new()), |list| {
                    let logins = list
                        .items
                        .iter()
                        .filter(|(_, item)| item.item_type == nox_core::ItemType::Login)
                        .count();
                    let notes = list
                        .items
                        .iter()
                        .filter(|(_, item)| item.item_type == nox_core::ItemType::SecureNote)
                        .count();
                    let recent = list
                        .items
                        .iter()
                        .rev()
                        .take(5)
                        .map(|(id, item)| (*id, item.clone()))
                        .collect::<Vec<_>>();
                    (list.items.len(), logins, notes, recent)
                });
        let locker = cx.entity();
        let add = self.render_add_item_menu(cx);
        let view_all = Button::new("home-view-all")
            .ghost()
            .h(px(26.))
            .label("View all ›")
            .text_color(theme.text_secondary)
            .on_click({
                let locker = locker.clone();
                move |_, _window, cx| {
                    locker.update(cx, |locker, cx| {
                        locker.set_active_view(ActiveView::AllItems, cx)
                    });
                }
            });
        let recent_rows: Vec<AnyElement> = recent
            .into_iter()
            .map(|(item_id, item)| {
                let item_key = crate::icons::item_key(item_id);
                let local_selection = self.local_icon_selections.get(&item_key);
                let resolved_icon = crate::icons::resolved_item_icon(
                    &self.data_dir,
                    &item_key,
                    &item,
                    local_selection,
                );
                let title = if item.title.is_empty() {
                    "Untitled".to_owned()
                } else {
                    item.title.clone()
                };
                let subtitle = recent_item_subtitle(&item);
                let time = relative_time(item.updated_at);
                let row_locker = locker.clone();
                Button::new(SharedString::from(format!("home-recent-{item_id}")))
                    .ghost()
                    .w_full()
                    .h(px(64.))
                    .justify_start()
                    .px(px(18.))
                    .border_b_1()
                    .border_color(theme.border)
                    .on_click(move |_, window, cx| {
                        row_locker.update(cx, |locker, cx| {
                            locker.open_editor_for_item(item_id, false, window, cx)
                        });
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .w_full()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(14.))
                                    .child(
                                        div()
                                            .size(px(34.))
                                            .rounded(px(8.))
                                            .bg(theme.raised)
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(crate::icons::render_resolved_icon(
                                                resolved_icon,
                                                15.,
                                                theme.text_secondary,
                                            )),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap(px(2.))
                                            .child(
                                                div().text_sm().text_color(theme.text).child(title),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(theme.text_muted)
                                                    .child(subtitle),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(16.))
                                    .child(div().text_xs().text_color(theme.text_muted).child(time))
                                    .child(
                                        gpui_component::Icon::empty()
                                            .path("icons/ellipsis-vertical.svg")
                                            .size(px(14.))
                                            .text_color(theme.text_muted),
                                    ),
                            ),
                    )
                    .into_any_element()
            })
            .collect();
        let recent_body = if recent_rows.is_empty() {
            div()
                .py(px(40.))
                .text_sm()
                .text_center()
                .text_color(theme.text_muted)
                .child("No items yet")
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .children(recent_rows)
                .into_any_element()
        };
        let new_login = home_quick_action(
            "home-new-login",
            "icons/key-square.svg",
            "New login",
            true,
            self.auth_hovered.get("home-new-login").copied(),
            {
                let locker = locker.clone();
                move |_, window, cx| {
                    locker.update(cx, |l, cx| {
                        if let Some(session) = l.session_mut() {
                            session.active_view = ActiveView::Logins;
                        }
                        l.open_create_editor(window, cx);
                    });
                }
            },
            cx,
        );
        let new_note = home_quick_action(
            "home-secure-note",
            "icons/file-lock.svg",
            "Secure note",
            true,
            self.auth_hovered.get("home-secure-note").copied(),
            {
                let locker = locker.clone();
                move |_, window, cx| {
                    locker.update(cx, |l, cx| {
                        if let Some(session) = l.session_mut() {
                            session.active_view = ActiveView::SecureNotes;
                        }
                        l.open_create_editor(window, cx);
                    });
                }
            },
            cx,
        );
        let new_card = home_quick_action(
            "home-payment-card",
            "icons/credit-card.svg",
            "Payment card",
            false,
            None,
            |_, _, _| {},
            cx,
        );
        let new_identity = home_quick_action(
            "home-identity",
            "icons/user.svg",
            "Identity",
            false,
            None,
            |_, _, _| {},
            cx,
        );
        rsx! {
            <div id="home-workspace" flex flex_col flex_1 min_w={px(0.)} h_full bg={theme.canvas}>
                <div id="home-header" flex items_center justify_between h={px(88.)} px={px(32.)} flex_shrink_0 border_b_1 borderColor={theme.border}>
                    <div flex flex_col gap={px(3.)}>
                        <div text_xl fontWeight={FontWeight::SEMIBOLD} textColor={theme.text}>{"Home"}</div>
                        <div text_xs textColor={theme.text_muted}>{"Your vault at a glance"}</div>
                    </div>
                    <div flex items_center gap={px(10.)}>
                        {sync_status_pill(theme)}
                        {add}
                    </div>
                </div>
                <div id="home-dashboard" flex flex_col gap={px(20.)} p={px(32.)} overflow_y_scroll>
                    <div id="home-hero" flex items_start justify_between p={px(24.)} bg={theme.surface} rounded={px(10.)} border_1 border_color={theme.field_border}>
                        <div flex flex_col gap={px(8.)} w={px(360.)}>
                            <div text_xs fontWeight={FontWeight::BOLD} textColor={theme.text_muted}>{"WELCOME BACK"}</div>
                            <div text_lg fontWeight={FontWeight::SEMIBOLD} textColor={theme.text}>{"Your vault is secure"}</div>
                            <div text_sm textColor={theme.text_muted}>
                                {format!(
                                    "Stored locally and encrypted. {total} item{} in your vault.",
                                    if total == 1 { "" } else { "s" },
                                )}
                            </div>
                        </div>
                        <div flex gap={px(12.)}>
                            {home_stat_tile(theme, "VAULT ITEMS", total)}
                            {home_stat_tile(theme, "LOGINS", logins)}
                            {home_stat_tile(theme, "SECURE NOTES", notes)}
                        </div>
                    </div>
                    <div id="home-quick-actions" flex flex_col gap={px(12.)}>
                        <div flex items_center justify_between>
                            <div text_sm fontWeight={FontWeight::SEMIBOLD} textColor={theme.text}>{"Quick actions"}</div>
                            <div text_xs textColor={theme.text_muted}>{"Ctrl+P to search or create"}</div>
                        </div>
                        <div flex gap={px(12.)}>
                            {new_login}
                            {new_note}
                            {new_card}
                            {new_identity}
                        </div>
                    </div>
                    <div flex gap={px(20.)} items_start>
                        <div id="home-recent-items" flex flex_col flex_1 min_w={px(0.)} rounded={px(10.)} bg={theme.surface}>
                            <div flex items_center justify_between h={px(58.)} px={px(18.)} border_b_1 borderColor={theme.border}>
                                <div flex flex_col gap={px(2.)}>
                                    <div text_sm fontWeight={FontWeight::SEMIBOLD} textColor={theme.text}>{"Recent items"}</div>
                                    <div text_xs textColor={theme.text_muted}>{"Newest first"}</div>
                                </div>
                                {view_all}
                            </div>
                            {recent_body}
                        </div>
                        <div flex flex_col gap={px(16.)} w={px(330.)} flex_shrink_0>
                            <div flex flex_col gap={px(10.)} p={px(18.)} rounded={px(10.)} bg={theme.surface}>
                                <div flex items_center gap={px(8.)}>
                                    {gpui_component::Icon::empty().path("icons/shield-check.svg").size(px(15.)).text_color(theme.text_secondary)}
                                    <div text_sm fontWeight={FontWeight::SEMIBOLD} textColor={theme.text}>{"Security health"}</div>
                                </div>
                                <div text_xs textColor={theme.text_muted}>{"Password health scoring isn't available yet."}</div>
                            </div>
                            <div flex flex_col gap={px(10.)} p={px(18.)} rounded={px(10.)} bg={theme.surface}>
                                <div flex items_center gap={px(8.)}>
                                    {gpui_component::Icon::empty().path("icons/star.svg").size(px(15.)).text_color(theme.text_secondary)}
                                    <div text_sm fontWeight={FontWeight::SEMIBOLD} textColor={theme.text}>{"Favorites"}</div>
                                </div>
                                <div text_xs textColor={theme.text_muted}>{"Favoriting items isn't available yet."}</div>
                            </div>
                        </div>
                    </div>
                </div>
            </div>
        }.into_any_element()
    }
}
