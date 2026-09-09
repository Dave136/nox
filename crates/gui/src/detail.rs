//! Read-only detail panel for the currently selected vault item: shows its
//! fields and lets the user copy the username/password by clicking the row.
//! Matches the Pencil "All Items Detail Pane" frame — both states: "No
//! Selection" (nothing picked yet) and the filled layout below.

use crate::app::Nox;
use crate::clipboard::CopyField;
use crate::item_editor::{
    NotePreviewBlockKind, NotePreviewSpan, NotePreviewSpanStyle,
    secure_note_markdown_preview_blocks,
};
use crate::theme::{APP_FONT_FAMILY, Theme};
use crate::vault_list;
use gpui::{
    AnyElement, App, ClickEvent, Context, FontWeight, Hsla, SharedString, Window, div, prelude::*,
    px,
};
use gpui_component::{
    Icon, IconName, Sizable,
    button::{Button, ButtonVariants as _},
};
use nox_core::{ItemId, ItemType, NoteColor};

impl Nox {
    pub(crate) fn render_item_detail(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::current(cx);
        let foreground: Hsla = theme.text;
        let muted_foreground: Hsla = theme.text_subtle;
        let accent: Hsla = theme.text_secondary;

        let card = |content: AnyElement| {
            div()
                .id("item-detail-panel")
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(0.))
                .h_full()
                .rounded(px(9.))
                .bg(theme.surface)
                .border_1()
                .border_color(theme.border)
                .overflow_hidden()
                .child(content)
        };

        let Some((item_id, payload)) = self.session().and_then(|session| {
            let item_id = session.list.selected?;
            session
                .list
                .items
                .iter()
                .find(|(id, _)| *id == item_id)
                .map(|(_, payload)| (item_id, payload.clone()))
        }) else {
            return card(empty_panel(theme)).into_any_element();
        };

        let feedback = self.clipboard.feedback;
        let reveal_password = self
            .session()
            .is_some_and(|session| session.reveal_password);
        let locker = cx.entity();
        let title = if payload.title.is_empty() {
            "Untitled".to_owned()
        } else {
            payload.title.clone()
        };
        let local_selection = self
            .local_icon_selections
            .get(&crate::icons::item_key(item_id));
        let icon = crate::icons::render_resolved_icon(
            crate::icons::resolved_item_icon(
                &self.data_dir,
                &crate::icons::item_key(item_id),
                &payload,
                local_selection,
            ),
            18.,
            if payload.item_type == ItemType::SecureNote && payload.note_color != NoteColor::Neutral
            {
                crate::icons::note_color_hsla(payload.note_color)
            } else {
                accent
            },
        );

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
                            .bg(
                                if payload.item_type == ItemType::SecureNote
                                    && payload.note_color != NoteColor::Neutral
                                {
                                    crate::icons::note_color_wash_hsla(payload.note_color)
                                } else {
                                    theme.raised
                                },
                            )
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon),
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
            let copy_note_button = Button::new("copy-note-contents")
                .h(px(40.))
                .w_full()
                .rounded(px(7.))
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
                                .text_color(theme.canvas),
                        )
                        .child(
                            div()
                                .text_size(px(10.))
                                .font_weight(FontWeight(650.))
                                .text_color(theme.canvas)
                                .child(if copied {
                                    "Copied!"
                                } else {
                                    "Copy note contents"
                                }),
                        ),
                );
            body = body.child(crate::app::animated_auth_button(
                "copy-note-contents",
                copy_note_button,
                self.auth_hovered.get("copy-note-contents").copied(),
                (
                    theme.inverse,
                    theme.inverse_hover,
                    theme.inverse_press,
                    theme.on_inverse,
                ),
                cx,
            ));
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
                let open_website = Button::new("detail-open-website")
                    .h(px(38.))
                    .w_full()
                    .rounded(px(7.))
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
                                    .text_color(theme.canvas),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.canvas)
                                    .child(format!("Open {host}")),
                            ),
                    );
                body = body.child(crate::app::animated_auth_button(
                    "detail-open-website",
                    open_website,
                    self.auth_hovered.get("detail-open-website").copied(),
                    (
                        theme.inverse,
                        theme.inverse_hover,
                        theme.inverse_press,
                        theme.on_inverse,
                    ),
                    cx,
                ));
            }

            let username_locker = locker.clone();
            body = body.child(copy_row(
                theme,
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
                theme.text, // bright: copy is the only action here
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
                        .text_color(theme.text_subtle),
                )
                .on_click(move |_, _window, app| {
                    reveal_locker.update(app, |locker, cx| {
                        if let Some(session) = locker.session_mut() {
                            session.reveal_password = !session.reveal_password;
                        }
                        cx.notify();
                    });
                });
            let password_locker = locker.clone();
            body = body.child(copy_row(
                theme,
                "detail-password",
                "PASSWORD",
                password_display,
                feedback == Some((item_id, CopyField::Password)),
                foreground,
                accent,
                theme.text, // bright: copy is the primary action, reveal is secondary
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
                            .text_color(theme.text),
                    )
                    .on_click(move |_, _window, app| app.open_url(&uri_for_open));
                let uri_for_copy = uri.clone();
                let uri_locker = locker.clone();
                body = body.child(copy_row(
                    theme,
                    SharedString::from(format!("detail-website-{index}")),
                    "WEBSITE",
                    uri.clone(),
                    feedback == Some((item_id, CopyField::Uri(index))),
                    foreground,
                    accent,
                    theme.text_subtle, // muted: opening is the primary action here
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
            body = body.child(self.render_secure_note_detail_content(
                item_id,
                &payload.notes,
                theme,
                locker.clone(),
            ));
        } else if !payload.notes.is_empty() {
            body = body.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(field_label(theme, "Notes"))
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
            .session()
            .map(|session| vault_list::duplicate_passwords(&session.list.items))
            .unwrap_or_default();
        let mut metadata = div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .pt(px(4.))
            .border_t_1()
            .border_color(theme.border)
            .child(metadata_row(
                theme,
                "Last modified",
                crate::app::relative_time(payload.updated_at),
            ))
            .child(metadata_row(
                theme,
                "Created",
                absolute_date(payload.created_at),
            ));
        if payload.item_type == ItemType::Login {
            let (health_label, health_color) = vault_list::login_health(theme, &payload, &dupes);
            metadata = metadata.child(metadata_row_colored(
                theme,
                "Password health",
                health_label,
                health_color,
            ));
        } else {
            metadata = metadata.child(metadata_row_colored(
                theme,
                "Protection",
                "End-to-end encrypted",
                theme.success,
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
                    theme,
                    "detail-edit-item",
                    "icons/pencil.svg",
                    edit_label,
                    theme.text_secondary,
                    move |_, window, app| {
                        edit_locker.update(app, |locker, cx| {
                            locker.open_editor_for_item(item_id, window, cx)
                        });
                    },
                ))
                .child(footer_button(
                    theme,
                    "detail-duplicate-item",
                    "icons/copy-plus.svg",
                    duplicate_label,
                    theme.text_secondary,
                    move |_, _window, app| {
                        duplicate_locker
                            .update(app, |locker, cx| locker.duplicate_item(item_id, cx));
                    },
                ))
                .child(footer_button(
                    theme,
                    "detail-delete-item",
                    "icons/trash-2.svg",
                    delete_label,
                    theme.danger,
                    move |_, window, app| {
                        delete_locker.update(app, |locker, cx| {
                            locker.open_delete_confirmation(item_id, window, cx)
                        });
                    },
                )),
        );

        card(body.into_any_element()).into_any_element()
    }

    fn render_secure_note_detail_content(
        &self,
        item_id: ItemId,
        notes: &str,
        theme: Theme,
        locker: gpui::Entity<Self>,
    ) -> AnyElement {
        let preview_spans = |spans: Vec<NotePreviewSpan>, text_color, text_size| {
            div()
                .flex()
                .flex_wrap()
                .gap(px(0.))
                .children(spans.into_iter().map(move |span| {
                    div()
                        .text_size(text_size)
                        .text_color(text_color)
                        .font_family(APP_FONT_FAMILY)
                        .when(span.style == NotePreviewSpanStyle::Bold, |span| {
                            span.font_weight(FontWeight::BOLD)
                        })
                        .when(span.style == NotePreviewSpanStyle::Italic, |span| {
                            span.italic()
                        })
                        .when(span.style == NotePreviewSpanStyle::Code, |span| {
                            span.px(px(4.))
                                .rounded(px(4.))
                                .bg(theme.inset)
                                .text_color(theme.text_secondary)
                        })
                        .child(span.text)
                }))
                .into_any_element()
        };

        let preview_blocks = secure_note_markdown_preview_blocks(notes)
            .into_iter()
            .enumerate()
            .map(|(index, block)| {
                let block_id = SharedString::from(format!("secure-note-markdown-preview-block-{index}"));
                match block.kind {
                    NotePreviewBlockKind::CopyBlock => {
                        let copy_text = block.copy_text.clone().unwrap_or_default();
                        let label = block.label.clone();
                        let locked = block.locked;
                        let revealed = self.detail_revealed_copy_blocks.contains(&(item_id, index));
                        let copy_locker = locker.clone();
                        let reveal_locker = locker.clone();
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .when_some(label.clone(), |container, label| {
                                container.child(
                                    div()
                                        .text_size(px(10.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme.text_soft)
                                        .child(label),
                                )
                            })
                            .child(
                                div()
                                    .id(block_id)
                                    .flex()
                                    .flex_col()
                                    .gap(px(10.))
                                    .p(px(12.))
                                    .rounded(px(8.))
                                    .bg(theme.inset)
                                    .border_1()
                                    .border_color(theme.field_border)
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(8.))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w(px(0.))
                                                    .text_size(px(11.))
                                                    .text_color(theme.text_subtle)
                                                    .child(if locked && !revealed {
                                                        div()
                                                            .text_size(px(12.))
                                                            .font_weight(FontWeight::SEMIBOLD)
                                                            .text_color(theme.text_secondary)
                                                            .child("••••••••••••••••")
                                                            .into_any_element()
                                                    } else {
                                                        preview_spans(block.spans, theme.text_subtle, px(11.))
                                                    }),
                                            )
                                            .when(locked, |row| {
                                                row.child(
                                                    Button::new(SharedString::from(format!(
                                                        "secure-note-copy-block-reveal-{index}"
                                                    )))
                                                    .ghost()
                                                    .size(px(26.))
                                                    .rounded(px(6.))
                                                    .tooltip(if revealed { "Hide" } else { "Reveal" })
                                                    .on_click(move |_, _window, app| {
                                                        reveal_locker.update(app, |locker, cx| {
                                                            locker.toggle_detail_secure_note_copy_block_reveal(
                                                                item_id, index, cx,
                                                            );
                                                        });
                                                    })
                                                    .child(
                                                        Icon::empty()
                                                            .path(if revealed {
                                                                "icons/scan-eye.svg"
                                                            } else {
                                                                "icons/eye-off.svg"
                                                            })
                                                            .size(px(14.))
                                                            .text_color(theme.icon_muted),
                                                    ),
                                                )
                                            })
                                            .child(
                                                Button::new(SharedString::from(format!(
                                                    "secure-note-copy-block-copy-{index}"
                                                )))
                                                .ghost()
                                                .h(px(26.))
                                                .px(px(8.))
                                                .rounded(px(6.))
                                                .tooltip("Copy block")
                                                .on_click(move |_, window, app| {
                                                    let copy_text = copy_text.clone();
                                                    copy_locker.update(app, |locker, cx| {
                                                        locker.copy_secure_note_block(copy_text, window, cx);
                                                    });
                                                })
                                                .child(
                                                    Icon::empty()
                                                        .path("icons/copy.svg")
                                                        .size(px(12.))
                                                        .text_color(theme.text_secondary),
                                                ),
                                            ),
                                    ),
                            )
                            .into_any_element()
                    }
                    NotePreviewBlockKind::Heading => div()
                        .id(block_id)
                        .text_size(px(18.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child(preview_spans(block.spans, theme.text, px(18.)))
                        .into_any_element(),
                    NotePreviewBlockKind::ListItem => div()
                        .id(block_id)
                        .flex()
                        .gap(px(8.))
                        .text_size(px(11.))
                        .text_color(theme.text_soft)
                        .child(
                            div()
                                .mt(px(7.))
                                .size(px(4.))
                                .rounded_full()
                                .bg(theme.text_secondary),
                        )
                        .child(preview_spans(block.spans, theme.text_soft, px(11.)))
                        .into_any_element(),
                    NotePreviewBlockKind::Code => div()
                        .id(block_id)
                        .p(px(10.))
                        .rounded(px(7.))
                        .bg(theme.inset)
                        .border_1()
                        .border_color(theme.field_border)
                        .text_size(px(10.))
                        .text_color(theme.text_secondary)
                        .child(preview_spans(block.spans, theme.text_secondary, px(10.)))
                        .into_any_element(),
                    NotePreviewBlockKind::Paragraph => div()
                        .id(block_id)
                        .text_size(px(11.))
                        .line_height(px(18.))
                        .text_color(theme.text_subtle)
                        .child(preview_spans(block.spans, theme.text_subtle, px(11.)))
                        .into_any_element(),
                }
            })
            .collect::<Vec<_>>();

        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(field_label(theme, "NOTE CONTENT"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(5.))
                            .h(px(22.))
                            .px(px(7.))
                            .rounded(px(5.))
                            .bg(theme.success_wash)
                            .child(
                                Icon::empty()
                                    .path("icons/lock-keyhole.svg")
                                    .size(px(10.))
                                    .text_color(theme.success),
                            )
                            .child(
                                div()
                                    .text_size(px(8.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.success)
                                    .child("ENCRYPTED"),
                            ),
                    ),
            )
            .child(
                div()
                    .min_h(px(112.))
                    .p(px(14.))
                    .rounded(px(8.))
                    .bg(theme.field)
                    .border_1()
                    .border_color(theme.border)
                    .text_size(px(12.))
                    .text_color(theme.text_soft)
                    .when(notes.is_empty(), |content| {
                        content.child("No note contents")
                    })
                    .when(!notes.is_empty(), |content| {
                        content
                            .flex()
                            .flex_col()
                            .gap(px(12.))
                            .children(preview_blocks)
                    }),
            )
            .into_any_element()
    }

    fn toggle_detail_secure_note_copy_block_reveal(
        &mut self,
        item_id: ItemId,
        block_index: usize,
        cx: &mut Context<Self>,
    ) {
        let key = (item_id, block_index);
        if !self.detail_revealed_copy_blocks.insert(key) {
            self.detail_revealed_copy_blocks.remove(&key);
        }
        cx.notify();
    }
}

// ponytail: the design also specifies 0.7px letter-spacing on these labels,
// which GPUI has no API for (same limitation as the sidebar's section labels).
fn field_label(theme: Theme, text: &'static str) -> impl IntoElement {
    div()
        .text_size(px(9.))
        .font_weight(FontWeight(700.))
        .text_color(theme.icon_muted)
        .child(text)
}

fn metadata_row(theme: Theme, label: &'static str, value: String) -> AnyElement {
    metadata_row_colored(theme, label, &value, theme.text_secondary)
}

fn metadata_row_colored(
    theme: Theme,
    label: &'static str,
    value: &str,
    value_color: Hsla,
) -> AnyElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .child(div().text_xs().text_color(theme.text_subtle).child(label))
        .child(
            div()
                .text_xs()
                .text_color(value_color)
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
    theme: Theme,
    id: &'static str,
    icon_path: &'static str,
    label: &'static str,
    color: Hsla,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    Button::new(id)
        .flex_1()
        .h(px(34.))
        .rounded(px(7.))
        .bg(theme.raised)
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
                        .text_color(color),
                )
                .child(div().text_size(px(12.)).text_color(color).child(label)),
        )
        .into_any_element()
}

/// The "No Selection Empty State": matches the Pencil frame exactly — an
/// icon badge, title/description, a keyboard-navigation hint, and a search
/// tip. Every affordance it describes (↑↓/Enter row navigation, Ctrl+P
/// search) is real, already-wired behavior, not a promise of a future one.
fn empty_panel(theme: Theme) -> AnyElement {
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
                        .bg(theme.raised)
                        .border_1()
                        .border_color(theme.field_border)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            gpui_component::Icon::empty()
                                .path("icons/mouse-pointer-2.svg")
                                .size(px(22.))
                                .text_color(theme.text_secondary),
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
                                .text_color(theme.text)
                                .child("Select an item"),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_center()
                                .text_color(theme.text_subtle)
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
                        .bg(theme.inset)
                        .border_1()
                        .border_color(theme.border)
                        .child(
                            gpui_component::Icon::empty()
                                .path("icons/keyboard.svg")
                                .size(px(14.))
                                .text_color(theme.icon_muted),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(theme.icon_muted)
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
                                .text_color(theme.text_ghost),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.text_ghost)
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
    theme: Theme,
    id: impl Into<SharedString>,
    label: &'static str,
    value: String,
    copied: bool,
    foreground: Hsla,
    accent: Hsla,
    copy_icon_color: Hsla,
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
            .text_color(copy_icon_color)
            .with_size(px(14.))
            .into_any_element()
    };
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(field_label(theme, label))
        .child(
            Button::new(id.into())
                .ghost()
                .w_full()
                .h(px(40.))
                .px(px(10.))
                .rounded(px(7.))
                .bg(theme.field)
                .border_1()
                .border_color(theme.border)
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

    #[test]
    fn secure_note_detail_renders_markdown_preview_blocks() {
        let source = include_str!("detail.rs");
        let secure_note_start = source
            .find("if payload.item_type == ItemType::SecureNote")
            .expect("secure note branch");
        let secure_note_branch = &source[secure_note_start
            ..source[secure_note_start..]
                .find("} else if !payload.notes.is_empty()")
                .expect("secure note branch end")
                + secure_note_start];

        assert!(secure_note_branch.contains("render_secure_note_detail_content"));
        assert!(source.contains("secure_note_markdown_preview_blocks(notes)"));
        assert!(source.contains("NotePreviewBlockKind::CopyBlock"));
        assert!(source.contains("secure-note-copy-block-copy-{index}"));
        assert!(!secure_note_branch.contains(".child(payload.notes.clone())"));
    }
}
