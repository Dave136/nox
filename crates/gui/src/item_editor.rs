use super::{AppState, Nox, nav::ActiveView};
use gpui::{
    AnyElement, Context, Entity, FontWeight, SharedString, Window, div, prelude::*, px, rgb,
};
use gpui_component::{
    ActiveTheme, Disableable, Icon, Sizable, WindowExt,
    button::{Button, ButtonVariants as _},
    checkbox::Checkbox,
    input::{Input, InputState, Textarea, TextareaState},
    popover::Popover,
    radio::{Radio, RadioGroup},
};
use gpui_rsx::rsx;
use nox_core::{
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

fn sheet_title(mode: EditorMode, item_type: ItemType) -> &'static str {
    match (mode, item_type) {
        (EditorMode::Create, ItemType::Login) => "New login",
        (EditorMode::Create, ItemType::SecureNote) => "New secure note",
        (EditorMode::Edit(_), ItemType::Login) => "Edit login",
        (EditorMode::Edit(_), ItemType::SecureNote) => "Edit secure note",
        (EditorMode::Restore(_), _) => "Restore item",
    }
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
    cx: &mut Context<Nox>,
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
    cx: &mut Context<Nox>,
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
    pub(crate) fn for_create(window: &mut Window, cx: &mut Context<Nox>) -> Self {
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
        cx: &mut Context<Nox>,
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
        cx: &mut Context<Nox>,
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
        cx: &mut Context<Nox>,
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

    fn payload(&self, window: &mut Window, cx: &mut Context<Nox>) -> ItemPayload {
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

/// A crude 0–4 length+variety heuristic, not a real entropy estimate —
/// upgrade to something like zxcvbn if that's ever asked for. Matches the
/// same spirit as `vault_list::is_weak_password`, just live/granular instead
/// of a single weak/not-weak cutoff.
fn password_strength_score(password: &str) -> usize {
    if password.is_empty() {
        return 0;
    }
    let len = password.chars().count();
    let class_count = [
        password.chars().any(|c| c.is_ascii_lowercase()),
        password.chars().any(|c| c.is_ascii_uppercase()),
        password.chars().any(|c| c.is_ascii_digit()),
        password.chars().any(|c| !c.is_ascii_alphanumeric()),
    ]
    .into_iter()
    .filter(|met| *met)
    .count();
    let mut score = 1;
    if len >= 10 {
        score += 1;
    }
    if len >= 14 {
        score += 1;
    }
    if class_count >= 3 {
        score += 1;
    }
    score.min(4)
}

/// Whether this draft password already belongs to a saved login — checked
/// against the real vault, not a fabricated signal.
fn password_is_reused(password: &str, items: &[(ItemId, ItemPayload)]) -> bool {
    !password.is_empty()
        && items.iter().any(|(_, payload)| {
            payload.item_type == ItemType::Login && payload.password == password
        })
}

impl Nox {
    pub(crate) fn uses_secure_note_workspace(&self) -> bool {
        self.active_view == ActiveView::SecureNotes
            && matches!(
                self.item_editor,
                Some(ItemEditorState {
                    mode: EditorMode::Create,
                    item_type: ItemType::SecureNote,
                    ..
                })
            )
    }

    /// Mirrors `uses_secure_note_workspace`: a dedicated full-page create flow
    /// instead of the generic Sheet, matching the Pencil "Nox — Create Login"
    /// frame.
    pub(crate) fn uses_login_workspace(&self) -> bool {
        self.active_view == ActiveView::Logins
            && matches!(
                self.item_editor,
                Some(ItemEditorState {
                    mode: EditorMode::Create,
                    item_type: ItemType::Login,
                    ..
                })
            )
    }

    pub(crate) fn cancel_item_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.item_editor = None;
        window.close_sheet(cx);
        cx.notify();
    }

    /// Open the shadcn-style Sheet (drawer) that hosts the create/edit/restore form.
    ///
    /// The Sheet's content builder is invoked by `Root::render_sheet_layer`
    /// from *inside* `Nox`'s own render pass, so it cannot call
    /// `Entity::update`/`read` on `Nox` (it is already leased for that
    /// render and would panic). Instead `render_unlocked` refreshes
    /// `item_editor_sheet_cell` with freshly rendered content on every pass,
    /// and this closure just reads whatever is currently sitting in the cell.
    pub(crate) fn open_item_editor_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cell = self.item_editor_sheet_cell.clone();
        let locker_for_close = cx.entity();
        window.open_sheet(cx, move |sheet, _window, _cx| {
            let (title, content) = cell
                .borrow_mut()
                .take()
                .unwrap_or_else(|| ("Item".into(), div().into_any_element()));
            let locker_for_close = locker_for_close.clone();
            sheet
                .title(title)
                .child(content)
                .on_close(move |_, _window, app| {
                    locker_for_close.update(app, |locker, cx| {
                        locker.item_editor = None;
                        cx.notify();
                    });
                })
        });
    }

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
        if editor.title_input.read(cx).value().trim().is_empty() {
            if let Some(editor) = self.item_editor.as_mut() {
                editor.save_error = Some("Enter a title.".into());
            }
            cx.notify();
            return;
        }
        if editor.item_type == ItemType::SecureNote
            && editor.notes_input.read(cx).value().trim().is_empty()
        {
            if let Some(editor) = self.item_editor.as_mut() {
                editor.save_error = Some("Enter note content.".into());
            }
            cx.notify();
            return;
        }
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
                window.close_sheet(cx);
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
        window: &mut Window,
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
                window.close_sheet(cx);
            }
            Err(_) => {
                if let Some(editor) = self.item_editor.as_mut() {
                    editor.save_error = Some("Could not delete item. Try again.".into());
                }
            }
        }
        cx.notify();
    }

    /// Save a copy of `item_id` as a new item ("Title (copy)"), selecting the
    /// copy. Reuses the same `Vault::create_item` path a normal save does —
    /// no separate duplication machinery.
    pub(crate) fn duplicate_item(&mut self, item_id: ItemId, cx: &mut Context<Self>) {
        let Some(mut payload) = (match &self.state {
            AppState::Unlocked(vault) => vault.get_item(item_id).ok().flatten(),
            _ => None,
        }) else {
            return;
        };
        payload.title = format!("{} (copy)", payload.title);
        payload.created_at = now_millis();
        payload.updated_at = payload.created_at;
        let result = match &mut self.state {
            AppState::Unlocked(vault) => vault.create_item(&payload),
            _ => return,
        };
        match result {
            Ok(new_item_id) => {
                if let Some(list) = self.vault_list.as_mut() {
                    list.upsert(new_item_id, payload);
                    list.selected = Some(new_item_id);
                }
            }
            Err(error) => {
                if let Some(list) = self.vault_list.as_mut() {
                    list.load = super::vault_list::ListLoadState::Failed(
                        format!("Could not duplicate item: {error}").into(),
                    );
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

    /// Render the create/edit/restore form. Returns the Sheet title alongside
    /// the body since both are derived from the same editor state snapshot.
    pub(crate) fn render_item_editor(
        &mut self,
        locker: Entity<Nox>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (SharedString, AnyElement) {
        let theme = cx.theme();
        let border = theme.border;
        let foreground = theme.foreground;
        let muted_foreground = theme.muted_foreground;
        let danger = theme.danger;

        let Some(editor) = self.item_editor.as_ref() else {
            return (
                "Item".into(),
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(muted_foreground)
                    .child("Select an item or create a new one.")
                    .into_any_element(),
            );
        };
        let mode = editor.mode;
        let item_type = editor.item_type;
        let panel_title = sheet_title(mode, item_type);
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
            .trigger(
                Button::new("generate-password")
                    .ghost()
                    .xsmall()
                    .label("Generate…"),
            )
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
            .gap(px(18.))
            .py(px(16.))
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
        let content = content
            .child(
                div()
                    .flex()
                    .justify_end()
                    .pt(px(4.))
                    .border_t_1()
                    .border_color(border)
                    .child(
                        Button::new("save-item")
                            .primary()
                            .label(save_label)
                            .on_click({
                                let locker = locker.clone();
                                move |_, window, app| {
                                    locker.update(app, |locker, cx| locker.save_item(window, cx));
                                }
                            }),
                    ),
            )
            .into_any_element();
        (panel_title.into(), content)
    }

    pub(crate) fn render_secure_note_workspace(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(editor) = self.item_editor.as_ref() else {
            return div().into_any_element();
        };
        let title = editor.title_input.clone();
        let notes = editor.notes_input.clone();
        let character_count = notes.read(cx).value().chars().count();
        let save_error = editor.save_error.clone();
        let locker = cx.entity();

        let cancel_locker = locker.clone();
        let cancel = Button::new("cancel-secure-note")
            .h(px(38.))
            .px(px(14.))
            .rounded(px(8.))
            .bg(rgb(super::CIPHER_SURFACE))
            .border_1()
            .border_color(rgb(0x353C47))
            .on_click(move |_, window, app| {
                cancel_locker.update(app, |locker, cx| locker.cancel_item_editor(window, cx));
            })
            .child(
                div()
                    .text_size(px(12.))
                    .font_weight(FontWeight(500.))
                    .text_color(rgb(super::CIPHER_FOREGROUND_SOFT))
                    .child("Cancel"),
            );
        let save_locker = locker.clone();
        let save = Button::new("save-secure-note")
            .h(px(38.))
            .px(px(16.))
            .rounded(px(8.))
            .bg(rgb(super::CIPHER_PRIMARY))
            .on_click(move |_, window, app| {
                save_locker.update(app, |locker, cx| locker.save_item(window, cx));
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        Icon::empty()
                            .path("icons/check.svg")
                            .size(px(14.))
                            .text_color(rgb(super::CIPHER_BACKGROUND)),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(FontWeight(600.))
                            .text_color(rgb(super::CIPHER_BACKGROUND))
                            .child("Save note"),
                    ),
            );
        let back_locker = locker.clone();
        let back = Button::new("back-to-secure-notes")
            .ghost()
            .h(px(36.))
            .px(px(11.))
            .rounded(px(7.))
            .on_click(move |_, window, app| {
                back_locker.update(app, |locker, cx| locker.cancel_item_editor(window, cx));
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        Icon::empty()
                            .path("icons/arrow-left.svg")
                            .size(px(14.))
                            .text_color(rgb(super::CIPHER_FOREGROUND_SECONDARY)),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight(500.))
                            .text_color(rgb(super::CIPHER_FOREGROUND_SOFT))
                            .child("Back to secure notes"),
                    ),
            );
        let field = |label: &'static str, required: bool, body: AnyElement| {
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(9.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(0x737E8D))
                                .child(label),
                        )
                        .when(required, |row| {
                            row.child(
                                div()
                                    .text_size(px(9.))
                                    .text_color(rgb(super::CIPHER_DISABLED))
                                    .child("Required"),
                            )
                        }),
                )
                .child(body)
        };
        let tool = |id: &'static str, icon: &'static str| {
            Button::new(id).ghost().size(px(26.)).rounded(px(6.)).child(
                Icon::empty()
                    .path(icon)
                    .size(px(14.))
                    .text_color(rgb(0x737E8D)),
            )
        };
        let note_editor = div()
            .flex()
            .flex_col()
            .h(px(220.))
            .rounded(px(7.))
            .bg(rgb(0x20242A))
            .border_1()
            .border_color(rgb(0x353C47))
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(38.))
                    .px(px(10.))
                    .gap(px(4.))
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(rgb(0x353C47))
                    .child(tool("note-format-bold", "icons/bold.svg"))
                    .child(tool("note-format-italic", "icons/italic.svg"))
                    .child(tool("note-format-list", "icons/list.svg"))
                    .child(tool("note-format-code", "icons/code.svg"))
                    .child(div().w(px(1.)).h(px(16.)).mx(px(4.)).bg(rgb(0x353C47)))
                    .child(
                        div()
                            .text_size(px(9.))
                            .text_color(rgb(super::CIPHER_DISABLED))
                            .child("Markdown supported"),
                    ),
            )
            .child(
                Textarea::new(&notes)
                    .appearance(false)
                    .bordered(false)
                    .flex_1()
                    .min_h(px(0.))
                    .p(px(12.))
                    .text_color(rgb(super::CIPHER_FOREGROUND_SOFT)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(30.))
                    .px(px(12.))
                    .flex_shrink_0()
                    .border_t_1()
                    .border_color(rgb(0x353C47))
                    .child(
                        div()
                            .text_size(px(9.))
                            .text_color(rgb(super::CIPHER_FOREGROUND_SUBTLE))
                            .child("Draft saved locally"),
                    )
                    .child(
                        div()
                            .text_size(px(9.))
                            .text_color(rgb(super::CIPHER_FOREGROUND_SECONDARY))
                            .child(format!("{character_count} characters")),
                    ),
            )
            .into_any_element();
        let outcome = |icon: &'static str, heading: &'static str, detail: &'static str| {
            div()
                .flex()
                .gap(px(10.))
                .child(
                    div()
                        .size(px(28.))
                        .flex_shrink_0()
                        .rounded(px(6.))
                        .bg(rgb(super::CIPHER_SURFACE_RAISED))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            Icon::empty()
                                .path(icon)
                                .size(px(13.))
                                .text_color(rgb(super::CIPHER_FOREGROUND_SECONDARY)),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(3.))
                        .child(
                            div()
                                .text_size(px(10.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(super::CIPHER_FOREGROUND_SOFT))
                                .child(heading),
                        )
                        .child(
                            div()
                                .text_size(px(9.))
                                .text_color(rgb(super::CIPHER_FOREGROUND_SUBTLE))
                                .child(detail),
                        ),
                )
        };
        let error = save_error.map_or_else(
            || div().into_any_element(),
            |message| {
                div()
                    .text_size(px(11.))
                    .text_color(rgb(super::CIPHER_DANGER))
                    .child(message)
                    .into_any_element()
            },
        );

        rsx! {
            <div id="secure-note-workspace" flex flex_col flex_1 min_w={px(0.)} h_full bg={rgb(super::CIPHER_BACKGROUND)}>
                <div flex items_center justify_between h={px(88.)} px={px(32.)} flex_shrink_0 border_b_1 borderColor={rgb(0x292D35)}>
                    <div flex flex_col gap={px(3.)}>
                        <div text_xl fontWeight={FontWeight::SEMIBOLD} textColor={rgb(super::CIPHER_FOREGROUND)}>{"Create secure note"}</div>
                        <div text_xs textColor={rgb(super::CIPHER_FOREGROUND_MUTED)}>{"Add an encrypted note to your vault"}</div>
                    </div>
                    <div flex items_center gap={px(10.)}>{cancel}{save}</div>
                </div>
                <div flex flex_col flex_1 min_h={px(0.)} p={px(28.)} pt={px(20.)} gap={px(16.)} overflow_y_scroll>
                    <div flex items_center justify_between h={px(40.)} flex_shrink_0>
                        {back}
                        <div flex items_center gap={px(8.)}>
                            {Icon::empty().path("icons/shield-check.svg").size(px(14.)).text_color(rgb(super::CIPHER_FOREGROUND_SUBTLE))}
                            <div text_size={px(9.)} textColor={rgb(0x737E8D)}>{"Encrypted locally · not saved yet"}</div>
                        </div>
                    </div>
                    <div flex flex_1 min_h={px(0.)} gap={px(16.)}>
                        <div flex flex_col w={px(760.)} flex_shrink_0 h_full p={px(24.)} gap={px(14.)} rounded={px(9.)} bg={rgb(super::CIPHER_SURFACE)} border_1 borderColor={rgb(super::CIPHER_BORDER)}>
                            <div flex items_center justify_between h={px(44.)} flex_shrink_0>
                                <div flex flex_col gap={px(4.)}>
                                    <div text_size={px(14.)} fontWeight={FontWeight::SEMIBOLD} textColor={rgb(super::CIPHER_FOREGROUND)}>{"Note details"}</div>
                                    <div text_size={px(10.)} textColor={rgb(super::CIPHER_FOREGROUND_SUBTLE)}>{"Store sensitive text securely, end-to-end encrypted."}</div>
                                </div>
                                <div flex items_center h={px(26.)} px={px(8.)} gap={px(6.)} rounded={px(6.)} bg={rgb(super::CIPHER_SURFACE_RAISED)} border_1 borderColor={rgb(0x353C47)}>
                                    {Icon::empty().path("icons/file-lock.svg").size(px(12.)).text_color(rgb(super::CIPHER_FOREGROUND_SECONDARY))}
                                    <div text_size={px(9.)} fontWeight={FontWeight::SEMIBOLD} textColor={rgb(super::CIPHER_FOREGROUND_SECONDARY)}>{"NOTE"}</div>
                                </div>
                            </div>
                            {field("TITLE", true, Input::new(&title).h(px(42.)).px(px(11.)).bg(rgb(0x20242A)).border_color(rgb(0x353C47)).rounded(px(7.)).prefix(Icon::empty().path("icons/notebook-pen.svg").size(px(14.)).text_color(rgb(0x737E8D))).into_any_element())}
                            {field("CONTENT", true, note_editor)}
                            {error}
                            <div flex_1 />
                            <div flex items_center h={px(42.)} px={px(11.)} rounded={px(7.)} bg={rgb(0x191C21)} border_1 borderColor={rgb(0x2B3039)}>
                                {Icon::empty().path("icons/lock-keyhole.svg").size(px(13.)).text_color(rgb(0x8DB49D))}
                                <div ml={px(8.)} text_size={px(9.)} textColor={rgb(super::CIPHER_FOREGROUND_SUBTLE)}>{"Encrypted before it leaves this device"}</div>
                                <div ml_auto text_size={px(9.)} fontWeight={FontWeight::SEMIBOLD} textColor={rgb(super::CIPHER_FOREGROUND_SECONDARY)}>{"⌘ ↵  Save note"}</div>
                            </div>
                        </div>
                        <div flex flex_col flex_1 min_w={px(0.)} h_full gap={px(14.)}>
                            <div flex flex_col p={px(20.)} gap={px(14.)} rounded={px(9.)} bg={rgb(super::CIPHER_SURFACE)} border_1 borderColor={rgb(super::CIPHER_BORDER)}>
                                <div flex items_center justify_between>
                                    <div flex items_center gap={px(9.)}>
                                        <div size={px(30.)} flex items_center justify_center rounded={px(7.)} bg={rgb(super::CIPHER_SURFACE_RAISED)}>{Icon::empty().path("icons/shield-check.svg").size(px(15.)).text_color(rgb(super::CIPHER_FOREGROUND_SECONDARY))}</div>
                                        <div flex flex_col gap={px(2.)}><div text_size={px(12.)} fontWeight={FontWeight::SEMIBOLD} textColor={rgb(super::CIPHER_FOREGROUND)}>{"Note privacy"}</div><div text_size={px(9.)} textColor={rgb(super::CIPHER_DISABLED)}>{"Encrypted the moment you type"}</div></div>
                                    </div>
                                    <div h={px(24.)} px={px(8.)} flex items_center rounded={px(6.)} bg={rgb(super::CIPHER_SURFACE_RAISED)} text_size={px(8.)} fontWeight={FontWeight::SEMIBOLD} textColor={rgb(0x8FBF9A)}>{"ENCRYPTED"}</div>
                                </div>
                                <div text_size={px(10.)} textColor={rgb(super::CIPHER_FOREGROUND_SUBTLE)}>{"Notes are encrypted locally before they ever leave this device, and stay unreadable without your master password."}</div>
                                <div flex flex_col gap={px(9.)} text_size={px(10.)} textColor={rgb(super::CIPHER_FOREGROUND_MUTED)}>
                                    <div flex items_center gap={px(8.)}>{Icon::empty().path("icons/key-round.svg").size(px(13.)).text_color(rgb(super::CIPHER_DISABLED))}{"Wi-Fi passwords and PINs"}</div>
                                    <div flex items_center gap={px(8.)}>{Icon::empty().path("icons/key-round.svg").size(px(13.)).text_color(rgb(super::CIPHER_DISABLED))}{"Recovery and backup codes"}</div>
                                    <div flex items_center gap={px(8.)}>{Icon::empty().path("icons/shield-check.svg").size(px(13.)).text_color(rgb(super::CIPHER_DISABLED))}{"Security question answers"}</div>
                                </div>
                            </div>
                            <div flex flex_col p={px(20.)} gap={px(13.)} rounded={px(9.)} bg={rgb(super::CIPHER_SURFACE)} border_1 borderColor={rgb(super::CIPHER_BORDER)}>
                                <div text_size={px(12.)} fontWeight={FontWeight::SEMIBOLD} textColor={rgb(super::CIPHER_FOREGROUND)}>{"After saving"}</div>
                                {outcome("icons/copy-plus.svg", "Quick copy", "The note content becomes a quick copy target.")}
                                {outcome("icons/search.svg", "Full-text search", "Find this note instantly across your vault.")}
                                {outcome("icons/refresh-cw.svg", "Sync securely", "The encrypted note syncs with your vault devices.")}
                            </div>
                            <div flex_1 />
                            <div flex items_center justify_between h={px(70.)} px={px(16.)} rounded={px(9.)} bg={rgb(0x191C21)} border_1 borderColor={rgb(super::CIPHER_BORDER)}>
                                <div flex flex_col gap={px(3.)}><div text_size={px(10.)} fontWeight={FontWeight::SEMIBOLD} textColor={rgb(super::CIPHER_FOREGROUND_SOFT)}>{"Keyboard friendly"}</div><div text_size={px(9.)} textColor={rgb(super::CIPHER_DISABLED)}>{"Tab between fields · Esc to cancel"}</div></div>
                                <div text_size={px(11.)} fontWeight={FontWeight::SEMIBOLD} textColor={rgb(super::CIPHER_FOREGROUND_SECONDARY)}>{"⌘ ↵"}</div>
                            </div>
                        </div>
                    </div>
                </div>
            </div>
        }
        .into_any_element()
    }

    /// The full-page "Create login" workspace (Pencil "Nox — Create Login").
    /// Reuses the same `ItemEditorState`/generator/save machinery as the
    /// Sheet-based editor — only the layout and chrome are new.
    ///
    /// Deliberate omissions, since nothing in the data model backs them:
    /// Folder and Tags fields (no categorization/tagging concept at all —
    /// unlike Cards/IDs elsewhere, there's no reasonable "real 0" to show),
    /// "Add to favorites" (no favorite flag), and "Not found in known
    /// breaches" (would require sending password data to a network service —
    /// not something to wire in silently for a security tool).
    pub(crate) fn render_login_workspace(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(editor) = self.item_editor.as_ref() else {
            return div().into_any_element();
        };
        let title = editor.title_input.clone();
        let username = editor.username_input.clone();
        let password = editor.password_input.clone();
        let uris = editor.uris_input.clone();
        let notes = editor.notes_input.clone();
        let password_value = password.read(cx).value().to_string();
        let generated = editor
            .generator
            .generated
            .as_ref()
            .map(|password| String::from_utf8_lossy(password.as_bytes()).into_owned());
        let length_input = editor.generator.length_input.clone();
        let classes = editor.generator.classes;
        let generator_open = editor.generator.open;
        let save_error = editor.save_error.clone();

        let items = self
            .vault_list
            .as_ref()
            .map_or(Vec::new(), |list| list.items.clone());
        let strength = password_strength_score(&password_value);
        let has_min_length = password_value.chars().count() >= 14;
        let is_reused = password_is_reused(&password_value, &items);
        let has_password = !password_value.is_empty();

        let locker = cx.entity();

        let back_locker = locker.clone();
        let back_link = Button::new("back-to-logins")
            .ghost()
            .h(px(32.))
            .px(px(4.))
            .on_click(move |_, window, app| {
                back_locker.update(app, |locker, cx| locker.cancel_item_editor(window, cx));
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(13.))
                    .text_color(rgb(super::CIPHER_FOREGROUND_SOFT))
                    .child("‹")
                    .child("Back to logins"),
            );

        let cancel_locker = locker.clone();
        let cancel_button = Button::new("cancel-create-login")
            .h(px(38.))
            .px(px(16.))
            .rounded(px(8.))
            .bg(rgb(super::CIPHER_SURFACE))
            .on_click(move |_, window, app| {
                cancel_locker.update(app, |locker, cx| locker.cancel_item_editor(window, cx));
            })
            .child(
                div()
                    .text_size(px(14.))
                    .font_weight(FontWeight(500.))
                    .text_color(rgb(super::CIPHER_FOREGROUND_SOFT))
                    .child("Cancel"),
            );

        let save_locker = locker.clone();
        let save_button_base = Button::new("save-create-login")
            .h(px(38.))
            .px(px(16.))
            .rounded(px(8.))
            .on_click(move |_, window, app| {
                save_locker.update(app, |locker, cx| locker.save_item(window, cx));
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        gpui_component::Icon::empty()
                            .path("icons/check.svg")
                            .size(px(14.))
                            .text_color(rgb(super::CIPHER_BACKGROUND)),
                    )
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight(500.))
                            .text_color(rgb(super::CIPHER_BACKGROUND))
                            .child("Save login"),
                    ),
            );
        let save_button = super::animated_auth_button(
            "save-create-login",
            save_button_base,
            self.auth_hovered.get("save-create-login").copied(),
            (
                super::CIPHER_PRIMARY,
                0xF0F2F6,
                0xCDD2DC,
                super::CIPHER_BACKGROUND,
            ),
            cx,
        );

        let field = |label: &'static str,
                     required: bool,
                     helper: Option<&'static str>,
                     body: AnyElement| {
            div()
                .flex()
                .flex_col()
                .gap(px(7.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(10.))
                                .font_weight(FontWeight(600.))
                                .text_color(rgb(0x737E8D))
                                .child(label),
                        )
                        .child(if required {
                            div()
                                .text_size(px(11.))
                                .text_color(rgb(super::CIPHER_DISABLED))
                                .child("Required")
                                .into_any_element()
                        } else if let Some(helper) = helper {
                            div()
                                .text_size(px(11.))
                                .text_color(rgb(super::CIPHER_DISABLED))
                                .child(helper)
                                .into_any_element()
                        } else {
                            div().into_any_element()
                        }),
                )
                .child(body)
                .into_any_element()
        };
        let dark_input_style = |input: Input| {
            input
                .h(px(42.))
                .bg(rgb(0x20242A))
                .border_color(rgb(0x20242A))
                .rounded(px(8.))
        };

        let generate_open_locker = locker.clone();
        let generate_use_locker = locker.clone();
        let generate_run_locker = locker.clone();
        let generate_class_locker = locker.clone();
        let class_checkbox = move |id: &'static str, label: &'static str, class: CharClasses| {
            let locker = generate_class_locker.clone();
            Checkbox::new(id)
                .label(label)
                .checked(classes.contains(class))
                .on_click(move |checked, _, app| {
                    locker.update(app, |locker, cx| {
                        locker.set_generator_class(class, *checked, cx)
                    });
                })
        };
        let generate_trigger = Button::new("generate-password-trigger")
            .h(px(28.))
            .px(px(10.))
            .rounded(px(6.))
            .bg(rgb(super::CIPHER_FOREGROUND))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        gpui_component::Icon::empty()
                            .path("icons/wand-sparkles.svg")
                            .size(px(12.))
                            .text_color(rgb(super::CIPHER_BACKGROUND)),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(FontWeight(500.))
                            .text_color(rgb(super::CIPHER_BACKGROUND))
                            .child("Generate"),
                    ),
            );
        let generator_popover = Popover::new("create-login-password-generator")
            .trigger(generate_trigger)
            .open(generator_open)
            .on_open_change(move |open, _, app| {
                generate_open_locker.update(app, |locker, cx| locker.set_generator_open(*open, cx));
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
                                "cl-generator-lower",
                                "Lowercase",
                                CharClasses::LOWER,
                            ))
                            .child(class_checkbox(
                                "cl-generator-upper",
                                "Uppercase",
                                CharClasses::UPPER,
                            ))
                            .child(class_checkbox(
                                "cl-generator-digits",
                                "Digits",
                                CharClasses::DIGITS,
                            ))
                            .child(class_checkbox(
                                "cl-generator-symbols",
                                "Symbols",
                                CharClasses::SYMBOLS,
                            )),
                    )
                    .child(
                        div()
                            .font_weight(FontWeight::NORMAL)
                            .truncate()
                            .child(preview),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(
                                Button::new("cl-regenerate-password")
                                    .outline()
                                    .small()
                                    .label("Generate")
                                    .disabled(classes.is_empty())
                                    .on_click({
                                        let locker = generate_run_locker.clone();
                                        move |_, _, app| {
                                            locker.update(app, |locker, cx| {
                                                locker.generate_editor_password(cx)
                                            });
                                        }
                                    }),
                            )
                            .child(
                                Button::new("cl-use-generated-password")
                                    .primary()
                                    .small()
                                    .label("Use this password")
                                    .on_click({
                                        let locker = generate_use_locker.clone();
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

        // Live segments: neutral fill count, not a red/green judgment — matches
        // the design's understated language rather than an alarming meter.
        let strength_bars = div().flex().gap(px(6.)).children((0..4).map(|index| {
            div()
                .flex_1()
                .h(px(5.))
                .rounded(px(3.))
                .bg(rgb(if index < strength { 0x525B69 } else { 0x282D35 }))
        }));
        let requirement = |met: bool, label: &'static str| {
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(if met {
                    gpui_component::Icon::empty()
                        .path("icons/check.svg")
                        .size(px(12.))
                        .text_color(rgb(super::CIPHER_FOREGROUND_SECONDARY))
                        .into_any_element()
                } else {
                    div()
                        .size(px(12.))
                        .rounded_full()
                        .border_1()
                        .border_color(rgb(super::CIPHER_DISABLED))
                        .into_any_element()
                })
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(rgb(if met {
                            super::CIPHER_FOREGROUND_SECONDARY
                        } else {
                            super::CIPHER_FOREGROUND_MUTED
                        }))
                        .child(label),
                )
        };
        let after_saving_row =
            |icon_path: &'static str, title: &'static str, description: &'static str| {
                div()
                    .flex()
                    .items_start()
                    .gap(px(12.))
                    .child(
                        div()
                            .size(px(28.))
                            .flex_shrink_0()
                            .rounded(px(8.))
                            .bg(rgb(super::CIPHER_SURFACE_RAISED))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                gpui_component::Icon::empty()
                                    .path(icon_path)
                                    .size(px(13.))
                                    .text_color(rgb(super::CIPHER_FOREGROUND_SECONDARY)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .text_color(rgb(super::CIPHER_FOREGROUND_SOFT))
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(rgb(super::CIPHER_FOREGROUND_SUBTLE))
                                    .child(description),
                            ),
                    )
            };

        let error = save_error.map(|message| {
            div()
                .text_sm()
                .text_color(rgb(super::CIPHER_DANGER))
                .child(message)
        });

        let form_card = div()
            .id("create-login-form")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .rounded(px(10.))
            .bg(rgb(super::CIPHER_SURFACE))
            .overflow_y_scroll()
            .p(px(24.))
            .gap(px(24.))
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(super::CIPHER_FOREGROUND))
                                    .child("Account details"),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(rgb(super::CIPHER_FOREGROUND_SUBTLE))
                                    .child("Store credentials and sign-in information securely."),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .h(px(26.))
                            .px(px(10.))
                            .rounded(px(6.))
                            .bg(rgb(super::CIPHER_SURFACE_RAISED))
                            .child(
                                gpui_component::Icon::empty()
                                    .path("icons/key-square.svg")
                                    .size(px(12.))
                                    .text_color(rgb(super::CIPHER_FOREGROUND_SECONDARY)),
                            )
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .font_weight(FontWeight(700.))
                                    .text_color(rgb(super::CIPHER_FOREGROUND_SECONDARY))
                                    .child("LOGIN"),
                            ),
                    ),
            )
            .child(field(
                "NAME",
                true,
                None,
                dark_input_style(Input::new(&title).aria_label("Login name")).into_any_element(),
            ))
            .child(field(
                "USERNAME",
                false,
                Some("Click row later to copy"),
                dark_input_style(Input::new(&username).aria_label("Username")).into_any_element(),
            ))
            .child(field(
                "PASSWORD",
                true,
                None,
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        dark_input_style(
                            Input::new(&password).mask_toggle().aria_label("Password"),
                        )
                        .flex_1(),
                    )
                    .child(generator_popover)
                    .into_any_element(),
            ))
            .child(field(
                "WEBSITES",
                false,
                Some("One per line"),
                Textarea::new(&uris)
                    .bg(rgb(0x20242A))
                    .border_color(rgb(0x20242A))
                    .rounded(px(8.))
                    .into_any_element(),
            ))
            .child(field(
                "NOTES",
                false,
                None,
                Textarea::new(&notes)
                    .bg(rgb(0x20242A))
                    .border_color(rgb(0x20242A))
                    .rounded(px(8.))
                    .into_any_element(),
            ))
            .children(error)
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(38.))
                    .px(px(14.))
                    .rounded(px(8.))
                    .bg(rgb(0x191C21))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(super::CIPHER_FOREGROUND_SUBTLE))
                            .child("Encrypted before it leaves this device"),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(super::CIPHER_FOREGROUND_SECONDARY))
                            .child("Ctrl + Enter to save"),
                    ),
            )
            .into_any_element();

        let guidance_card = div()
            .flex()
            .flex_col()
            .w(px(384.))
            .flex_shrink_0()
            .gap(px(16.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    .p(px(20.))
                    .rounded(px(10.))
                    .bg(rgb(super::CIPHER_SURFACE))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(12.))
                                    .child(
                                        div()
                                            .size(px(30.))
                                            .rounded(px(8.))
                                            .bg(rgb(super::CIPHER_SURFACE_RAISED))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(
                                                gpui_component::Icon::empty()
                                                    .path("icons/shield-check.svg")
                                                    .size(px(15.))
                                                    .text_color(rgb(
                                                        super::CIPHER_FOREGROUND_SECONDARY,
                                                    )),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap(px(2.))
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(rgb(super::CIPHER_FOREGROUND))
                                                    .child("Password health"),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(11.))
                                                    .text_color(rgb(super::CIPHER_DISABLED))
                                                    .child("Updates as you type"),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .h(px(22.))
                                    .px(px(9.))
                                    .rounded(px(6.))
                                    .bg(rgb(super::CIPHER_SURFACE_RAISED))
                                    .flex()
                                    .items_center()
                                    .child(
                                        div()
                                            .text_size(px(9.))
                                            .font_weight(FontWeight(700.))
                                            .text_color(rgb(super::CIPHER_FOREGROUND_SUBTLE))
                                            .child(if has_password { "LIVE" } else { "PENDING" }),
                                    ),
                            ),
                    )
                    .child(strength_bars)
                    .child(if has_password {
                        div().into_any_element()
                    } else {
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(super::CIPHER_FOREGROUND_SUBTLE))
                            .child("Enter a password or generate one to check its strength.")
                            .into_any_element()
                    })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(10.))
                            .child(requirement(
                                has_password && has_min_length,
                                "At least 14 characters",
                            ))
                            .child(requirement(
                                has_password && !is_reused,
                                "Unique and not reused",
                            ))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(
                                        div()
                                            .size(px(12.))
                                            .rounded_full()
                                            .border_1()
                                            .border_color(rgb(super::CIPHER_DISABLED)),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(13.))
                                            .text_color(rgb(super::CIPHER_FOREGROUND_MUTED))
                                            .child("Breach check unavailable offline"),
                                    ),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    .p(px(20.))
                    .rounded(px(10.))
                    .bg(rgb(super::CIPHER_SURFACE))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(super::CIPHER_FOREGROUND))
                            .child("After saving"),
                    )
                    .child(after_saving_row(
                        "icons/ellipsis-vertical.svg",
                        "One-click copy",
                        "Username and password rows become quick copy targets.",
                    ))
                    .child(after_saving_row(
                        "icons/external-link.svg",
                        "Open the website",
                        "The website row opens directly in your browser.",
                    ))
                    .child(after_saving_row(
                        "icons/cloud-check.svg",
                        "Stored encrypted",
                        "Ready to sync the moment you pair another device.",
                    )),
            )
            .into_any_element();

        div()
            .id("create-login-workspace")
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .bg(rgb(super::CIPHER_BACKGROUND))
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "escape" => this.cancel_item_editor(window, cx),
                    "enter"
                        if event.keystroke.modifiers.control
                            || event.keystroke.modifiers.platform =>
                    {
                        this.save_item(window, cx)
                    }
                    _ => {}
                }
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(88.))
                    .px(px(32.))
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(rgb(super::CIPHER_BORDER))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(3.))
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(super::CIPHER_FOREGROUND))
                                    .child("Create login"),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(rgb(super::CIPHER_FOREGROUND_MUTED))
                                    .child("Add a secure account to your vault"),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(cancel_button)
                            .child(save_button),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .p(px(28.))
                    .pt(px(20.))
                    .gap(px(16.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(back_link)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(
                                        gpui_component::Icon::empty()
                                            .path("icons/lock-keyhole.svg")
                                            .size(px(14.))
                                            .text_color(rgb(super::CIPHER_FOREGROUND_SUBTLE)),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(12.))
                                            .text_color(rgb(0x737E8D))
                                            .child("Encrypted locally · not saved yet"),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_h(px(0.))
                            .gap(px(16.))
                            .child(form_card)
                            .child(guidance_card),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved_login(password: &str) -> ItemPayload {
        ItemPayload {
            schema_version: ITEM_SCHEMA_VERSION,
            item_type: ItemType::Login,
            title: "Existing".into(),
            username: "alex".into(),
            password: password.into(),
            uris: vec![],
            notes: String::new(),
            created_at: 1,
            updated_at: 1,
        }
    }

    #[test]
    fn strength_score_rewards_length_and_variety_and_treats_empty_as_zero() {
        assert_eq!(password_strength_score(""), 0);
        assert_eq!(password_strength_score("short"), 1);
        assert_eq!(password_strength_score("longerpassword"), 3);
        assert_eq!(password_strength_score("Lo7ng3rP@ssword!"), 4);
    }

    #[test]
    fn reuse_check_only_matches_saved_login_passwords() {
        let items = vec![
            (ItemId::new(), saved_login("shared-secret")),
            (ItemId::new(), {
                let mut note = saved_login("note-body");
                note.item_type = ItemType::SecureNote;
                note
            }),
        ];
        assert!(password_is_reused("shared-secret", &items));
        assert!(!password_is_reused("note-body", &items));
        assert!(!password_is_reused("unused-password", &items));
        // Empty means "not set yet", not a reuse collision with other unset fields.
        assert!(!password_is_reused("", &items));
    }
}
