//! Read-only detail panel for the currently selected vault item: shows its
//! fields and lets the user copy the username/password by clicking the row.
//! Matches the Pencil "All Items Detail Pane" frame — both states: "No
//! Selection" (nothing picked yet) and the filled layout below.

use crate::app::Nox;
use crate::clipboard::CopyField;
use crate::vault_list;
use gpui::{
    AnyElement, App, ClickEvent, Context, FontWeight, Hsla, SharedString, Window, div, prelude::*,
    px, rgb,
};
use gpui_component::{
    Icon, IconName, Sizable,
    button::{Button, ButtonVariants as _},
};
use nox_core::ItemType;

impl Nox {
    pub(crate) fn render_item_detail(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let foreground: Hsla = rgb(crate::theme::CIPHER_FOREGROUND).into();
        let muted_foreground: Hsla = rgb(crate::theme::CIPHER_FOREGROUND_SUBTLE).into();
        let accent: Hsla = rgb(crate::theme::CIPHER_FOREGROUND_SECONDARY).into();

        let card = |content: AnyElement| {
            div()
                .id("item-detail-panel")
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(0.))
                .h_full()
                .rounded(px(9.))
                .bg(rgb(crate::theme::CIPHER_SURFACE))
                .border_1()
                .border_color(rgb(crate::theme::CIPHER_BORDER))
                .overflow_hidden()
                .child(content)
        };

        let Some((item_id, payload)) = self.vault_list.as_ref().and_then(|list| {
            let item_id = list.selected?;
            list.items
                .iter()
                .find(|(id, _)| *id == item_id)
                .map(|(_, payload)| (item_id, payload.clone()))
        }) else {
            return card(empty_panel()).into_any_element();
        };

        let feedback = self.clipboard.feedback;
        let reveal_password = self.reveal_password;
        let locker = cx.entity();
        let title = if payload.title.is_empty() {
            "Untitled".to_owned()
        } else {
            payload.title.clone()
        };
        let icon_path = match payload.item_type {
            ItemType::Login => "icons/key-square.svg",
            ItemType::SecureNote => "icons/file-lock.svg",
        };

        let mut body = div()
            .id("item-detail")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .overflow_y_scroll()
            .p(px(22.))
            .gap(px(20.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .size(px(40.))
                            .flex_shrink_0()
                            .rounded(px(9.))
                            .bg(rgb(if payload.item_type == ItemType::SecureNote {
                                0x282D35
                            } else {
                                crate::theme::CIPHER_SURFACE_RAISED
                            }))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                gpui_component::Icon::empty()
                                    .path(icon_path)
                                    .size(px(18.))
                                    .text_color(accent),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .min_w(px(0.))
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(foreground)
                                    .truncate()
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_size(px(if payload.item_type == ItemType::SecureNote {
                                        8.
                                    } else {
                                        12.
                                    }))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(muted_foreground)
                                    .child(match payload.item_type {
                                        ItemType::Login => "Login",
                                        ItemType::SecureNote => "SECURE NOTE",
                                    }),
                            ),
                    ),
            );

        if payload.item_type == ItemType::SecureNote {
            let copy_note_locker = locker.clone();
            let copied = feedback == Some((item_id, CopyField::Note));
            body = body.child(
                Button::new("copy-note-contents")
                    .h(px(40.))
                    .w_full()
                    .rounded(px(7.))
                    .bg(rgb(crate::theme::CIPHER_FOREGROUND))
                    .on_click(move |_, window, app| {
                        copy_note_locker
                            .update(app, |locker, cx| locker.copy_note(item_id, window, cx));
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap(px(8.))
                            .child(
                                gpui_component::Icon::empty()
                                    .path("icons/copy.svg")
                                    .size(px(14.))
                                    .text_color(rgb(crate::theme::CIPHER_BACKGROUND)),
                            )
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .font_weight(FontWeight(650.))
                                    .text_color(rgb(crate::theme::CIPHER_BACKGROUND))
                                    .child(if copied {
                                        "Copied!"
                                    } else {
                                        "Copy note contents"
                                    }),
                            ),
                    ),
            );
        }

        if payload.item_type == ItemType::Login {
            if let Some(uri) = payload.uris.first() {
                let host = uri
                    .trim_start_matches("https://")
                    .trim_start_matches("http://")
                    .split('/')
                    .next()
                    .filter(|host| !host.is_empty())
                    .unwrap_or(uri);
                let open_uri = uri.clone();
                body = body.child(
                    Button::new("detail-open-website")
                        .h(px(38.))
                        .w_full()
                        .rounded(px(7.))
                        .bg(rgb(crate::theme::CIPHER_PRIMARY))
                        .on_click(move |_, _window, app| app.open_url(&open_uri))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_center()
                                .gap(px(7.))
                                .child(
                                    gpui_component::Icon::empty()
                                        .path("icons/external-link.svg")
                                        .size(px(14.))
                                        .text_color(rgb(crate::theme::CIPHER_BACKGROUND)),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(rgb(crate::theme::CIPHER_BACKGROUND))
                                        .child(format!("Open {host}")),
                                ),
                        ),
                );
            }

            let username_locker = locker.clone();
            body = body.child(copy_row(
                "detail-username",
                "USERNAME",
                if payload.username.is_empty() {
                    "—".to_owned()
                } else {
                    payload.username.clone()
                },
                feedback == Some((item_id, CopyField::Username)),
                foreground,
                accent,
                crate::theme::CIPHER_FOREGROUND, // bright: copy is the only action here
                move |_, window, app| {
                    username_locker
                        .update(app, |locker, cx| locker.copy_username(item_id, window, cx));
                },
                None,
            ));

            let password_display = if payload.password.is_empty() {
                "—".to_owned()
            } else if reveal_password {
                payload.password.clone()
            } else {
                "•".repeat(payload.password.chars().count().clamp(8, 24))
            };
            let reveal_locker = locker.clone();
            let reveal_button = Button::new("toggle-reveal-password")
                .ghost()
                .xsmall()
                .icon(
                    gpui_component::Icon::empty()
                        .path(if reveal_password {
                            "icons/eye-off.svg"
                        } else {
                            "icons/eye.svg"
                        })
                        .text_color(rgb(crate::theme::CIPHER_FOREGROUND_SUBTLE)),
                )
                .on_click(move |_, _window, app| {
                    reveal_locker.update(app, |locker, cx| {
                        locker.reveal_password = !locker.reveal_password;
                        cx.notify();
                    });
                });
            let password_locker = locker.clone();
            body = body.child(copy_row(
                "detail-password",
                "PASSWORD",
                password_display,
                feedback == Some((item_id, CopyField::Password)),
                foreground,
                accent,
                crate::theme::CIPHER_FOREGROUND, // bright: copy is the primary action, reveal is secondary
                move |_, window, app| {
                    password_locker
                        .update(app, |locker, cx| locker.copy_password(item_id, window, cx));
                },
                Some(reveal_button.into_any_element()),
            ));

            for (index, uri) in payload.uris.iter().enumerate() {
                let uri_for_open = uri.clone();
                let open_button = Button::new(SharedString::from(format!("open-uri-{index}")))
                    .ghost()
                    .xsmall()
                    .icon(
                        gpui_component::Icon::empty()
                            .path("icons/external-link.svg")
                            .text_color(rgb(crate::theme::CIPHER_FOREGROUND)),
                    )
                    .on_click(move |_, _window, app| app.open_url(&uri_for_open));
                let uri_for_copy = uri.clone();
                let uri_locker = locker.clone();
                body = body.child(copy_row(
                    SharedString::from(format!("detail-website-{index}")),
                    "WEBSITE",
                    uri.clone(),
                    feedback == Some((item_id, CopyField::Uri(index))),
                    foreground,
                    accent,
                    crate::theme::CIPHER_FOREGROUND_SUBTLE, // muted: opening is the primary action here
                    move |_, window, app| {
                        let uri = uri_for_copy.clone();
                        uri_locker.update(app, |locker, cx| {
                            locker.copy_uri(item_id, index, uri, window, cx)
                        });
                    },
                    Some(open_button.into_any_element()),
                ));
            }
        }

        if payload.item_type == ItemType::SecureNote {
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(field_label("NOTE CONTENT"))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(5.))
                                    .h(px(22.))
                                    .px(px(7.))
                                    .rounded(px(5.))
                                    .bg(rgb(0x28322E))
                                    .child(
                                        gpui_component::Icon::empty()
                                            .path("icons/lock-keyhole.svg")
                                            .size(px(10.))
                                            .text_color(rgb(0x8DB49D)),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(8.))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(rgb(0x8DB49D))
                                            .child("ENCRYPTED"),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .min_h(px(112.))
                            .p(px(14.))
                            .rounded(px(8.))
                            .bg(rgb(0x20242A))
                            .border_1()
                            .border_color(rgb(crate::theme::CIPHER_BORDER))
                            .text_size(px(12.))
                            .text_color(rgb(crate::theme::CIPHER_FOREGROUND_SOFT))
                            .child(if payload.notes.is_empty() {
                                "No note contents".to_owned()
                            } else {
                                payload.notes.clone()
                            }),
                    ),
            );
        } else if !payload.notes.is_empty() {
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(field_label("Notes"))
                    .child(
                        div()
                            .text_sm()
                            .text_color(foreground)
                            .child(payload.notes.clone()),
                    ),
            );
        }

        // Real metadata: relative "last modified", an absolute creation
        // date, and — for logins — the same weak/reused check the Logins
        // view's smart filters use, computed against every saved login.
        let dupes = self
            .vault_list
            .as_ref()
            .map(|list| vault_list::duplicate_passwords(&list.items))
            .unwrap_or_default();
        let mut metadata = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .pt(px(4.))
            .border_t_1()
            .border_color(rgb(crate::theme::CIPHER_BORDER))
            .child(metadata_row(
                "Last modified",
                crate::app::relative_time(payload.updated_at),
            ))
            .child(metadata_row("Created", absolute_date(payload.created_at)));
        if payload.item_type == ItemType::Login {
            let (health_label, health_color) = vault_list::login_health(&payload, &dupes);
            metadata = metadata.child(metadata_row_colored(
                "Password health",
                health_label,
                health_color,
            ));
        } else {
            metadata = metadata.child(metadata_row_colored(
                "Protection",
                "End-to-end encrypted",
                0x8DB49D,
            ));
        }
        body = body.child(metadata);

        let edit_locker = cx.entity();
        let duplicate_locker = edit_locker.clone();
        let delete_locker = edit_locker.clone();
        let (edit_label, duplicate_label, delete_label) =
            if payload.item_type == ItemType::SecureNote {
                ("Edit note", "Duplicate note", "Delete note")
            } else {
                ("Edit", "Duplicate", "Delete")
            };
        body = body.child(div().flex_1()).child(
            div()
                .flex()
                .gap(px(8.))
                .child(footer_button(
                    "detail-edit-item",
                    "icons/pencil.svg",
                    edit_label,
                    crate::theme::CIPHER_FOREGROUND_SECONDARY,
                    move |_, window, app| {
                        edit_locker.update(app, |locker, cx| {
                            locker.open_editor_for_item(item_id, false, window, cx)
                        });
                    },
                ))
                .child(footer_button(
                    "detail-duplicate-item",
                    "icons/copy-plus.svg",
                    duplicate_label,
                    crate::theme::CIPHER_FOREGROUND_SECONDARY,
                    move |_, _window, app| {
                        duplicate_locker
                            .update(app, |locker, cx| locker.duplicate_item(item_id, cx));
                    },
                ))
                .child(footer_button(
                    "detail-delete-item",
                    "icons/trash-2.svg",
                    delete_label,
                    crate::theme::CIPHER_DANGER,
                    move |_, window, app| {
                        delete_locker.update(app, |locker, cx| {
                            locker.open_delete_confirmation(item_id, window, cx)
                        });
                    },
                )),
        );

        card(body.into_any_element()).into_any_element()
    }
}

// ponytail: the design also specifies 0.7px letter-spacing on these labels,
// which GPUI has no API for (same limitation as the sidebar's section labels).
fn field_label(text: &'static str) -> impl IntoElement {
    div()
        .text_size(px(9.))
        .font_weight(FontWeight(700.))
        .text_color(rgb(0x737E8D))
        .child(text)
}

fn metadata_row(label: &'static str, value: String) -> AnyElement {
    metadata_row_colored(label, &value, crate::theme::CIPHER_FOREGROUND_SECONDARY)
}

fn metadata_row_colored(label: &'static str, value: &str, value_color: u32) -> AnyElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_xs()
                .text_color(rgb(crate::theme::CIPHER_FOREGROUND_SUBTLE))
                .child(label),
        )
        .child(
            div()
                .text_xs()
                .text_color(rgb(value_color))
                .child(value.to_owned()),
        )
        .into_any_element()
}

/// `12 Jan 2024` — chrono handles the calendar math (leap years, month
/// lengths) correctly rather than hand-rolling it.
fn absolute_date(created_at_ms: u64) -> String {
    let datetime: chrono::DateTime<chrono::Local> =
        (std::time::UNIX_EPOCH + std::time::Duration::from_millis(created_at_ms)).into();
    datetime.format("%d %b %Y").to_string()
}

/// One of the equal-width Edit/Duplicate/Delete footer buttons. The Pencil
/// design originally gave these three different, non-full-width sizes; that
/// was corrected in the .pen file (and here) so all three share the row
/// evenly via `flex_1`.
fn footer_button(
    id: &'static str,
    icon_path: &'static str,
    label: &'static str,
    color: u32,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    Button::new(id)
        .flex_1()
        .h(px(34.))
        .rounded(px(7.))
        .bg(rgb(crate::theme::CIPHER_SURFACE_RAISED))
        .on_click(on_click)
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    gpui_component::Icon::empty()
                        .path(icon_path)
                        .size(px(13.))
                        .text_color(rgb(color)),
                )
                .child(div().text_size(px(12.)).text_color(rgb(color)).child(label)),
        )
        .into_any_element()
}

/// The "No Selection Empty State": matches the Pencil frame exactly — an
/// icon badge, title/description, a keyboard-navigation hint, and a search
/// tip. Every affordance it describes (↑↓/Enter row navigation, Ctrl+P
/// search) is real, already-wired behavior, not a promise of a future one.
fn empty_panel() -> AnyElement {
    div()
        .id("item-detail-empty")
        .flex()
        .flex_1()
        .h_full()
        .items_center()
        .justify_center()
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .w(px(300.))
                .gap(px(20.))
                .child(
                    div()
                        .size(px(52.))
                        .flex_shrink_0()
                        .rounded(px(12.))
                        .bg(rgb(crate::theme::CIPHER_SURFACE_RAISED))
                        .border_1()
                        .border_color(rgb(0x353C47))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            gpui_component::Icon::empty()
                                .path("icons/mouse-pointer-2.svg")
                                .size(px(22.))
                                .text_color(rgb(crate::theme::CIPHER_FOREGROUND_SECONDARY)),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap(px(6.))
                        .child(
                            div()
                                .text_lg()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(crate::theme::CIPHER_FOREGROUND))
                                .child("Select an item"),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_center()
                                .text_color(rgb(crate::theme::CIPHER_FOREGROUND_SUBTLE))
                                .child(
                                    "Choose an item from the list to view its details, reveal fields, or copy credentials.",
                                ),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .h(px(32.))
                        .px(px(11.))
                        .rounded(px(7.))
                        .bg(rgb(0x191C21))
                        .border_1()
                        .border_color(rgb(0x292D35))
                        .child(
                            gpui_component::Icon::empty()
                                .path("icons/keyboard.svg")
                                .size(px(14.))
                                .text_color(rgb(0x737E8D)),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(rgb(0x737E8D))
                                .child("↑↓ navigate · Enter open"),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(
                            gpui_component::Icon::empty()
                                .path("icons/search.svg")
                                .size(px(13.))
                                .text_color(rgb(crate::theme::CIPHER_DISABLED)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(crate::theme::CIPHER_DISABLED))
                                .child("Press Ctrl+P to search your vault"),
                        ),
                ),
        )
        .into_any_element()
}

/// One "USERNAME"/"PASSWORD"/"WEBSITE" field: matches the Pencil "Selected
/// Item Fields" frame — the label sits *above/outside* the value box, and
/// the box itself carries a `#2B3039` border (both were missing before).
/// The whole box still copies `value` on click; `copy_icon_color` lets the
/// copy glyph be muted when the field also has a `trailing` action that's
/// the more prominent one (e.g. Website's "open externally").
#[allow(clippy::too_many_arguments)]
fn copy_row(
    id: impl Into<SharedString>,
    label: &'static str,
    value: String,
    copied: bool,
    foreground: Hsla,
    accent: Hsla,
    copy_icon_color: u32,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    trailing: Option<AnyElement>,
) -> AnyElement {
    let status: AnyElement = if copied {
        div()
            .flex()
            .items_center()
            .gap(px(4.))
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .text_color(accent)
            .child(
                Icon::new(IconName::Check)
                    .text_color(accent)
                    .with_size(px(14.)),
            )
            .child("Copied")
            .into_any_element()
    } else {
        Icon::new(IconName::Copy)
            .text_color(rgb(copy_icon_color))
            .with_size(px(14.))
            .into_any_element()
    };
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(field_label(label))
        .child(
            Button::new(id.into())
                .ghost()
                .w_full()
                .h(px(40.))
                .px(px(10.))
                .rounded(px(7.))
                .bg(rgb(0x20242A))
                .border_1()
                .border_color(rgb(crate::theme::CIPHER_BORDER))
                .on_click(on_click)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .w_full()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .text_size(px(13.))
                                .text_color(foreground)
                                .truncate()
                                .child(value),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(10.))
                                .flex_shrink_0()
                                .children(trailing)
                                .child(status),
                        ),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    #[test]
    fn detail_card_fills_the_remaining_split_workspace_width() {
        let source = include_str!("detail.rs");
        let card_start = source.find("let card = |content").expect("detail card");
        let card = &source[card_start
            ..source[card_start..]
                .find("let Some((item_id, payload))")
                .expect("detail card end")
                + card_start];

        assert!(card.contains(".flex_1()"));
        assert!(card.contains(".min_w(px(0.))"));
        assert!(!card.contains(".w(px(428.))"));
    }

    #[test]
    fn secure_note_detail_renders_a_real_copy_contents_button() {
        let source = include_str!("detail.rs");
        assert!(source.contains("Button::new(\"copy-note-contents\")"));
        assert!(source.contains("locker.copy_note(item_id, window, cx)"));
    }
}
