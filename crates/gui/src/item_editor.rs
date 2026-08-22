use super::{AppState, Locker};
use gpui::{AnyElement, Context, Entity, FontWeight, SharedString, Window, div, prelude::*, px};
use gpui_component::{
    ActiveTheme, Disableable, Sizable, WindowExt,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    input::{Input, InputState, Textarea, TextareaState},
    popover::Popover,
    radio::{Radio, RadioGroup},
};
use locker_core::{
    CharClasses, ITEM_SCHEMA_VERSION, ItemId, ItemPayload, ItemType, MAX_LENGTH, Password, Vault,
    VaultError, generate_password,
};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EditorMode {
    Create,
    Edit(ItemId),
    Restore(ItemId),
}

pub(crate) struct GeneratorPopoverState {
    pub(crate) open: bool,
    pub(crate) length: usize,
    pub(crate) classes: CharClasses,
    pub(crate) generated: Option<Password>,
    pub(crate) length_input: Entity<InputState>,
}

pub(crate) struct ItemEditorState {
    pub(crate) mode: EditorMode,
    pub(crate) item_type: ItemType,
    pub(crate) title_input: Entity<InputState>,
    pub(crate) username_input: Entity<InputState>,
    pub(crate) password_input: Entity<InputState>,
    pub(crate) uris_input: Entity<TextareaState>,
    pub(crate) notes_input: Entity<TextareaState>,
    pub(crate) created_at: u64,
    pub(crate) save_error: Option<SharedString>,
    pub(crate) generator: GeneratorPopoverState,
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

fn input(
    value: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut Context<Locker>,
    placeholder: &'static str,
    masked: bool,
) -> Entity<InputState> {
    let value = value.into();
    let entity = cx.new(|cx| {
        let state = InputState::new(window, cx).placeholder(placeholder);
        if masked { state.masked(true) } else { state }
    });
    if !value.is_empty() {
        entity.update(cx, |state, input_cx| {
            state.set_value(value, window, input_cx)
        });
    }
    entity
}

fn textarea(
    value: impl Into<SharedString>,
    window: &mut Window,
    cx: &mut Context<Locker>,
    placeholder: &'static str,
) -> Entity<TextareaState> {
    let value = value.into();
    let entity = cx.new(|cx| {
        TextareaState::new(window, cx)
            .placeholder(placeholder)
            .rows(4)
    });
    if !value.is_empty() {
        entity.update(cx, |state, input_cx| {
            state.set_value(value, window, input_cx)
        });
    }
    entity
}

impl ItemEditorState {
    pub(crate) fn for_create(window: &mut Window, cx: &mut Context<Locker>) -> Self {
        let title_input = input("", window, cx, "Title", false);
        let username_input = input("", window, cx, "Username", false);
        let password_input = input("", window, cx, "Password", true);
        let uris_input = textarea("", window, cx, "URIs (one per line)");
        let notes_input = textarea("", window, cx, "Notes");
        let length_input = input("20", window, cx, "Length", false);
        Self {
            mode: EditorMode::Create,
            item_type: ItemType::Login,
            title_input,
            username_input,
            password_input,
            uris_input,
            notes_input,
            created_at: now_millis(),
            save_error: None,
            generator: GeneratorPopoverState {
                open: false,
                length: 20,
                classes: CharClasses::ALL,
                generated: None,
                length_input,
            },
        }
    }

    pub(crate) fn for_edit(
        item_id: ItemId,
        vault: &Vault,
        window: &mut Window,
        cx: &mut Context<Locker>,
    ) -> Result<Self, VaultError> {
        let payload = vault.get_item(item_id)?.ok_or(VaultError::ItemNotFound)?;
        Ok(Self::from_payload(
            EditorMode::Edit(item_id),
            payload,
            window,
            cx,
        ))
    }

    pub(crate) fn for_restore(
        item_id: ItemId,
        vault: &Vault,
        window: &mut Window,
        cx: &mut Context<Locker>,
    ) -> Result<Self, VaultError> {
        let payload = vault
            .last_known_payload(item_id)?
            .ok_or(VaultError::ItemNotFound)?;
        Ok(Self::from_payload(
            EditorMode::Restore(item_id),
            payload,
            window,
            cx,
        ))
    }

    fn from_payload(
        mode: EditorMode,
        payload: ItemPayload,
        window: &mut Window,
        cx: &mut Context<Locker>,
    ) -> Self {
        let mut editor = Self::for_create(window, cx);
        editor.mode = mode;
        editor.item_type = payload.item_type;
        editor.created_at = payload.created_at;
        editor.title_input.update(cx, |state, input_cx| {
            state.set_value(payload.title, window, input_cx)
        });
        editor.username_input.update(cx, |state, input_cx| {
            state.set_value(payload.username, window, input_cx)
        });
        editor.password_input.update(cx, |state, input_cx| {
            state.set_value(payload.password, window, input_cx)
        });
        editor.uris_input.update(cx, |state, input_cx| {
            state.set_value(payload.uris.join("\n"), window, input_cx)
        });
        editor.notes_input.update(cx, |state, input_cx| {
            state.set_value(payload.notes, window, input_cx)
        });
        editor
    }

    fn payload(&self, window: &mut Window, cx: &mut Context<Locker>) -> ItemPayload {
        let item_type = self.item_type;
        let title = self.title_input.read(cx).value().to_string();
        let username = if item_type == ItemType::Login {
            self.username_input.read(cx).value().to_string()
        } else {
            String::new()
        };
        let password = if item_type == ItemType::Login {
            self.password_input.read(cx).value().to_string()
        } else {
            String::new()
        };
        let uris = if item_type == ItemType::Login {
            self.uris_input
                .read(cx)
                .value()
                .lines()
                .map(str::trim)
                .filter(|uri| !uri.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        } else {
            Vec::new()
        };
        let _ = window;
        ItemPayload {
            schema_version: ITEM_SCHEMA_VERSION,
            item_type,
            title,
            username,
            password,
            uris,
            notes: self.notes_input.read(cx).value().to_string(),
            created_at: self.created_at,
            updated_at: now_millis(),
        }
    }
}

impl Locker {
    pub(crate) fn set_editor_type(&mut self, item_type: ItemType, cx: &mut Context<Self>) {
        if let Some(editor) = self.item_editor.as_mut() {
            editor.item_type = item_type;
            cx.notify();
        }
    }

    pub(crate) fn generate_editor_password(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.item_editor.as_mut() else {
            return;
        };
        if let Ok(length) = editor
            .generator
            .length_input
            .read(cx)
            .value()
            .parse::<usize>()
        {
            editor.generator.length = length.clamp(1, MAX_LENGTH);
        }
        editor.generator.generated =
            generate_password(editor.generator.length, editor.generator.classes).ok();
        cx.notify();
    }

    pub(crate) fn set_generator_class(
        &mut self,
        class: CharClasses,
        checked: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = self.item_editor.as_mut() {
            if checked {
                editor.generator.classes |= class;
            } else {
                let mut classes = CharClasses::EMPTY;
                for candidate in [
                    CharClasses::LOWER,
                    CharClasses::UPPER,
                    CharClasses::DIGITS,
                    CharClasses::SYMBOLS,
                ] {
                    if candidate != class && editor.generator.classes.contains(candidate) {
                        classes |= candidate;
                    }
                }
                editor.generator.classes = classes;
            }
            cx.notify();
        }
    }

    pub(crate) fn use_generated_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.item_editor.as_mut() else {
            return;
        };
        let Some(password) = editor.generator.generated.as_ref() else {
            return;
        };
        let value = String::from_utf8_lossy(password.as_bytes()).into_owned();
        editor.password_input.update(cx, |state, input_cx| {
            state.set_value(value, window, input_cx)
        });
        editor.generator.generated = None;
        editor.generator.open = false;
        cx.notify();
    }

    pub(crate) fn set_generator_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if let Some(editor) = self.item_editor.as_mut() {
            editor.generator.open = open;
            cx.notify();
        }
    }

    pub(crate) fn save_item(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.item_editor.as_ref() else {
            return;
        };
        let mode = editor.mode;
        let payload = editor.payload(window, cx);
        let result = match (&mut self.state, mode) {
            (AppState::Unlocked(vault), EditorMode::Create) => {
                vault.create_item(&payload).map(|item_id| (item_id, false))
            }
            (AppState::Unlocked(vault), EditorMode::Edit(item_id))
            | (AppState::Unlocked(vault), EditorMode::Restore(item_id)) => vault
                .update_item(item_id, &payload)
                .map(|()| (item_id, true)),
            _ => return,
        };
        match result {
            Ok((item_id, was_restore)) => {
                if let Some(list) = self.vault_list.as_mut() {
                    list.upsert(item_id, payload);
                    list.selected = Some(item_id);
                    if was_restore {
                        list.remove_deleted(item_id);
                    }
                }
                self.item_editor = None;
            }
            Err(_) => {
                if let Some(editor) = self.item_editor.as_mut() {
                    editor.save_error = Some("Could not save item. Try again.".into());
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn delete_item(
        &mut self,
        item_id: ItemId,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = match &mut self.state {
            AppState::Unlocked(vault) => vault.delete_item(item_id),
            _ => return,
        };
        match result {
            Ok(()) => {
                if let Some(list) = self.vault_list.as_mut() {
                    list.remove(item_id);
                    list.add_deleted(item_id);
                    list.selected = None;
                }
                self.item_editor = None;
            }
            Err(_) => {
                if let Some(editor) = self.item_editor.as_mut() {
                    editor.save_error = Some("Could not delete item. Try again.".into());
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn open_delete_confirmation(
        &mut self,
        item_id: ItemId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let locker = cx.entity().downgrade();
        let title = self
            .vault_list
            .as_ref()
            .and_then(|list| list.items.iter().find(|(id, _)| *id == item_id))
            .map_or_else(
                || "this item".to_owned(),
                |(_, payload)| payload.title.clone(),
            );
        window.open_alert_dialog(cx, move |dialog, _window, _cx| {
            let locker_for_ok = locker.clone();
            dialog
                .title("Delete item?")
                .child(format!(
                    "Delete \"{title}\"? This item will be moved to Deleted."
                ))
                .confirm()
                .on_ok(move |_, window, app| {
                    let _ = locker_for_ok
                        .update(app, |locker, cx| locker.delete_item(item_id, window, cx));
                    true
                })
                .on_cancel(|_, _, _| true)
        });
    }

    pub(crate) fn render_item_editor(
        &mut self,
        locker: Entity<Locker>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let border = theme.border;
        let foreground = theme.foreground;
        let muted_foreground = theme.muted_foreground;
        let danger = theme.danger;
        let card_bg = theme.background;
        let radius_lg = theme.radius_lg;

        let Some(editor) = self.item_editor.as_ref() else {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(muted_foreground)
                .child("Select an item or create a new one.")
                .into_any_element();
        };
        let mode = editor.mode;
        let item_type = editor.item_type;
        let title = editor.title_input.clone();
        let username = editor.username_input.clone();
        let password = editor.password_input.clone();
        let uris = editor.uris_input.clone();
        let notes = editor.notes_input.clone();
        let generated = editor
            .generator
            .generated
            .as_ref()
            .map(|password| String::from_utf8_lossy(password.as_bytes()).into_owned());
        let length_input = editor.generator.length_input.clone();
        let classes = editor.generator.classes;
        let generator_open = editor.generator.open;
        let selected_type = if item_type == ItemType::Login {
            Some(0)
        } else {
            Some(1)
        };
        let save_error = editor.save_error.clone();
        let save_label = if matches!(mode, EditorMode::Restore(_)) {
            "Restore"
        } else {
            "Save"
        };
        let locker_for_generate = locker.clone();
        let locker_for_use = locker.clone();
        let locker_for_type = locker.clone();
        let locker_for_open = locker.clone();
        let class_locker = locker.clone();
        let class_checkbox = move |id: &'static str, label: &'static str, class: CharClasses| {
            Checkbox::new(id)
                .label(label)
                .checked(classes.contains(class))
                .on_click({
                    let locker = class_locker.clone();
                    move |checked, _, app| {
                        locker.update(app, |locker, cx| {
                            locker.set_generator_class(class, *checked, cx)
                        });
                    }
                })
        };
        let generator = Popover::new("password-generator")
            .trigger(Button::new("generate-password").ghost().xsmall().label("Generate…"))
            .open(generator_open)
            .on_open_change(move |open, _, app| {
                locker_for_open.update(app, |locker, cx| locker.set_generator_open(*open, cx));
            })
            .content(move |_popover, _window, _cx| {
                let preview = generated.clone().unwrap_or_else(|| "Click Generate".into());
                div()
                    .p(px(12.))
                    .w(px(240.))
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Password generator")
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
                                Button::new("regenerate-password")
                                    .outline()
                                    .small()
                                    .label("Generate")
                                    .disabled(classes.is_empty())
                                    .on_click({
                                        let locker = locker_for_generate.clone();
                                        move |_, _, app| {
                                            locker.update(app, |locker, cx| {
                                                locker.generate_editor_password(cx)
                                            });
                                        }
                                    }),
                            )
                            .child(
                                Button::new("use-generated-password")
                                    .primary()
                                    .small()
                                    .label("Use this password")
                                    .on_click({
                                        let locker = locker_for_use.clone();
                                        move |_, window, app| {
                                            locker.update(app, |locker, cx| {
                                                locker.use_generated_password(window, cx)
                                            });
                                        }
                                    }),
                            ),
                    )
                    .into_any_element()
            });
        let type_group = RadioGroup::horizontal("item-type")
            .children([
                Radio::new("login-type").label("Login"),
                Radio::new("note-type").label("Secure note"),
            ])
            .selected_index(selected_type)
            .on_click(move |index, _, app| {
                let item_type = if *index == 0 {
                    ItemType::Login
                } else {
                    ItemType::SecureNote
                };
                locker_for_type.update(app, |locker, cx| locker.set_editor_type(item_type, cx));
            });

        let field_label = move |text: &'static str| {
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(foreground)
                .child(text)
        };
        let delete_button = if matches!(mode, EditorMode::Create) {
            div().into_any_element()
        } else {
            Button::new("delete-item")
                .danger()
                .outline()
                .small()
                .label("Delete")
                .on_click({
                    let locker = locker.clone();
                    move |_, window, app| {
                        locker.update(app, |locker, cx| {
                            if let Some(editor) = locker.item_editor.as_ref()
                                && let EditorMode::Edit(item_id) | EditorMode::Restore(item_id) =
                                    editor.mode
                            {
                                locker.open_delete_confirmation(item_id, window, cx);
                            }
                        });
                    }
                })
                .into_any_element()
        };

        let mut content = div()
            .id("item-editor")
            .flex()
            .flex_col()
            .gap(px(20.))
            .p(px(20.))
            .rounded(radius_lg)
            .border_1()
            .border_color(border)
            .bg(card_bg)
            .flex_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(type_group)
                    .child(delete_button),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(field_label("Title"))
                    .child(Input::new(&title)),
            );
        if item_type == ItemType::Login {
            let copy_buttons = match mode {
                EditorMode::Edit(item_id) | EditorMode::Restore(item_id) => div()
                    .flex()
                    .gap(px(8.))
                    .child(
                        Button::new("copy-username")
                            .ghost()
                            .xsmall()
                            .label("Copy username")
                            .on_click({
                                let locker = locker.clone();
                                move |_, window, app| {
                                    locker.update(app, |locker, cx| {
                                        locker.copy_username(item_id, window, cx)
                                    });
                                }
                            }),
                    )
                    .child(
                        Button::new("copy-password")
                            .ghost()
                            .xsmall()
                            .label("Copy password")
                            .on_click({
                                let locker = locker.clone();
                                move |_, window, app| {
                                    locker.update(app, |locker, cx| {
                                        locker.copy_password(item_id, window, cx)
                                    });
                                }
                            }),
                    ),
                EditorMode::Create => div(),
            };
            content = content
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(field_label("Username"))
                        .child(Input::new(&username)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(field_label("Password"))
                                .child(generator),
                        )
                        .child(Input::new(&password).mask_toggle()),
                )
                .child(copy_buttons)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(field_label("URIs"))
                        .child(Textarea::new(&uris)),
                );
        }
        content = content.child(
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(field_label("Notes"))
                .child(Textarea::new(&notes)),
        );
        if let Some(error) = save_error {
            content = content.child(div().text_sm().text_color(danger).child(error));
        }
        content
            .child(
                div()
                    .flex()
                    .justify_end()
                    .pt(px(4.))
                    .border_t_1()
                    .border_color(border)
                    .child(Button::new("save-item").primary().label(save_label).on_click({
                        let locker = locker.clone();
                        move |_, window, app| {
                            locker.update(app, |locker, cx| locker.save_item(window, cx));
                        }
                    })),
            )
            .into_any_element()
    }
}
