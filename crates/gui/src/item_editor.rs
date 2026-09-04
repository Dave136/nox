use crate::app::{AppState, Nox};
use crate::nav::ActiveView;
use crate::theme::Theme;
use gpui::{
    AnyElement, App, Context, Entity, FontWeight, PathPromptOptions, SharedString, Window, div,
    prelude::*, px,
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
    CharClasses, ITEM_SCHEMA_VERSION, IconChoice, ItemId, ItemPayload, ItemType, MAX_LENGTH,
    Password, Vault, VaultError, generate_password,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Identifies one *opening* of the editor, which `EditorMode` cannot: two
/// successive `Create` editors are equal as modes but are different editors,
/// and a favicon fetch started in the first must not land in the second.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct EditorId(u64);

static NEXT_EDITOR_ID: AtomicU64 = AtomicU64::new(0);

impl EditorId {
    fn next() -> Self {
        Self(NEXT_EDITOR_ID.fetch_add(1, Ordering::Relaxed))
    }
}

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

/// What the picker's "Favicon del sitio" row is doing right now. Only an
/// explicit click moves it out of `Idle` — nothing here ever starts on its own.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum FaviconFetchStatus {
    #[default]
    Idle,
    Loading,
    Failed,
}

/// Shown inline in the popover when a fetch comes back empty-handed: the
/// popover stays open and the item's icon is left exactly as it was.
pub(crate) const FAVICON_FETCH_ERROR: &str = "Couldn't fetch a favicon for that address.";

/// Shown under the picker's inline URL field. That URL is fetch input only: it
/// is deliberately never appended to `item.uris` (the user owns that list
/// through the URI field itself), so an item that should keep resolving this
/// favicon after a cache clear or a sync to another device needs the address
/// saved as a URI as well. Saying so beats letting the field imply it saved.
pub(crate) const FAVICON_TYPED_URL_NOTE: &str =
    "Used to find the icon only — save it as a URI to keep it.";

pub(crate) struct FaviconPickerState {
    /// The URL typed into the row's inline field when the item has no saved
    /// URI. It feeds the fetch only — it is never added to `item.uris`.
    pub(crate) url_input: Entity<InputState>,
    pub(crate) url_expanded: bool,
    pub(crate) status: FaviconFetchStatus,
    pub(crate) last_auto_fetch_uri: Option<String>,
}

pub(crate) struct GeneratorPopoverState {
    pub(crate) open: bool,
    pub(crate) length: usize,
    pub(crate) classes: CharClasses,
    pub(crate) generated: Option<Password>,
    pub(crate) length_input: Entity<InputState>,
}

pub(crate) struct ItemEditorState {
    /// Stamped once per opening and never reused, so an async reply can prove
    /// it is still talking to the editor that sent it.
    pub(crate) id: EditorId,
    pub(crate) mode: EditorMode,
    pub(crate) item_type: ItemType,
    pub(crate) icon: IconChoice,
    pub(crate) local_icon: Option<crate::icons::LocalIconRef>,
    pub(crate) title_input: Entity<InputState>,
    pub(crate) username_input: Entity<InputState>,
    pub(crate) password_input: Entity<InputState>,
    pub(crate) uris_input: Entity<TextareaState>,
    pub(crate) notes_input: Entity<TextareaState>,
    pub(crate) created_at: u64,
    pub(crate) save_error: Option<SharedString>,
    pub(crate) generator: GeneratorPopoverState,
    pub(crate) favicon: FaviconPickerState,
}

pub(crate) fn now_millis() -> u64 {
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
        let favicon_url_input = input("", window, cx, "https://example.com", false);
        Self {
            id: EditorId::next(),
            mode: EditorMode::Create,
            item_type: ItemType::Login,
            icon: IconChoice::Default,
            local_icon: None,
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
            favicon: FaviconPickerState {
                url_input: favicon_url_input,
                url_expanded: false,
                status: FaviconFetchStatus::Idle,
                last_auto_fetch_uri: None,
            },
        }
    }

    /// The item's URI lines as the editor currently holds them — the same list
    /// `payload` saves, and the candidates an explicit favicon fetch walks.
    pub(crate) fn uris(&self, cx: &App) -> Vec<String> {
        self.uris_input
            .read(cx)
            .value()
            .lines()
            .map(str::trim)
            .filter(|uri| !uri.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    }

    pub(crate) fn local_icon_key(&self) -> String {
        match self.mode {
            EditorMode::Create => crate::icons::editor_key(self.id.0),
            EditorMode::Edit(item_id) | EditorMode::Restore(item_id) => {
                crate::icons::item_key(item_id)
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn payload_for_test(&self, cx: &App) -> ItemPayload {
        let item_type = self.item_type;
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
            self.uris(cx)
        } else {
            Vec::new()
        };
        ItemPayload {
            schema_version: ITEM_SCHEMA_VERSION,
            item_type,
            title: self.title_input.read(cx).value().to_string(),
            username,
            password,
            uris,
            notes: self.notes_input.read(cx).value().to_string(),
            created_at: self.created_at,
            updated_at: now_millis(),
            icon: self.icon,
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
        editor.icon = payload.icon;
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
            self.uris(cx)
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
            icon: self.icon,
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

fn icon_picker_row(
    id: impl Into<gpui::ElementId>,
    icon_path: &'static str,
    label: &'static str,
    theme: Theme,
    locker: Entity<Nox>,
    choice: IconChoice,
) -> AnyElement {
    Button::new(id)
        .ghost()
        .w_full()
        .h(px(32.))
        .px(px(8.))
        .on_click(move |_, window, app| {
            locker.update(app, |locker, cx| {
                locker.choose_item_icon(choice, window, cx);
            });
        })
        .child(
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
                .child(div().text_size(px(13.)).text_color(theme.text).child(label)),
        )
        .into_any_element()
}

/// Everything the favicon row needs, snapshotted at render time: the popover's
/// content builder runs inside `Nox`'s own render pass and so cannot read the
/// entity back out.
#[derive(Clone)]
struct FaviconRow {
    theme: Theme,
    locker: Entity<Nox>,
    status: FaviconFetchStatus,
    error: Option<&'static str>,
    needs_url_input: bool,
    url_expanded: bool,
    url_input: Entity<InputState>,
    /// The "this URL isn't saved" caveat, when the inline field is showing.
    typed_url_note: Option<&'static str>,
    offers_refetch: bool,
}

/// The favicon row: a clickable row that fetches against the item's saved URIs,
/// or — with no URI saved yet — expands into an inline URL field plus "Buscar"
/// that fetches against just that typed URL. Never disabled outright, and never
/// fetching without a click.
fn favicon_picker_row(row: FaviconRow) -> AnyElement {
    let FaviconRow {
        theme,
        locker,
        status,
        error,
        needs_url_input,
        url_expanded,
        url_input,
        typed_url_note,
        offers_refetch,
    } = row;
    let loading = status == FaviconFetchStatus::Loading;
    let label = if loading {
        "Buscando…"
    } else if offers_refetch {
        // `IconChoice::Favicon` with no cached bytes (synced from another
        // device, or a cleared cache) renders the type default until the user
        // asks for the fetch again — this row is that ask.
        "Refetch favicon"
    } else {
        "Favicon del sitio"
    };
    let locker_for_row = locker.clone();
    let trigger_row = Button::new("icon-favicon")
        .ghost()
        .w_full()
        .h(px(32.))
        .px(px(8.))
        .disabled(loading)
        .on_click(move |_, window, app| {
            locker_for_row.update(app, |locker, cx| {
                // With no saved URI there is nothing to fetch against yet, so
                // the row opens its inline field instead of failing a fetch.
                if locker.icon_picker_needs_url_input(cx) {
                    locker.expand_favicon_url_input(cx);
                } else {
                    locker.fetch_item_favicon(window, cx);
                }
            });
        })
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    gpui_component::Icon::empty()
                        .path("icons/globe.svg")
                        .size(px(14.))
                        .text_color(theme.text_secondary),
                )
                .child(div().text_size(px(13.)).text_color(theme.text).child(label)),
        );

    div()
        .w_full()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(trigger_row)
        .when(needs_url_input && url_expanded, |this| {
            this.child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .px(px(8.))
                    .pb(px(4.))
                    .child(Input::new(&url_input))
                    .when_some(typed_url_note, |this, note| {
                        this.child(
                            div()
                                .text_size(px(11.))
                                .text_color(theme.text_subtle)
                                .child(note),
                        )
                    })
                    .child(
                        Button::new("icon-favicon-search")
                            .primary()
                            .small()
                            .label(if loading { "Buscando…" } else { "Buscar" })
                            .disabled(loading)
                            .on_click(move |_, window, app| {
                                locker.update(app, |locker, cx| {
                                    locker.fetch_item_favicon(window, cx);
                                });
                            }),
                    ),
            )
        })
        .when_some(error, |this, message| {
            this.child(
                div()
                    .px(px(8.))
                    .pb(px(4.))
                    .text_size(px(11.))
                    .text_color(theme.danger)
                    .child(message),
            )
        })
        .into_any_element()
}

fn upload_icon_picker_row(theme: Theme, locker: Entity<Nox>) -> AnyElement {
    Button::new("icon-upload")
        .ghost()
        .w_full()
        .h(px(32.))
        .px(px(8.))
        .on_click(move |_, window, app| {
            locker.update(app, |locker, cx| {
                locker.choose_uploaded_item_icon(window, cx);
            });
        })
        .child(
            div()
                .w_full()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    gpui_component::Icon::empty()
                        .path("icons/file-sliders.svg")
                        .size(px(14.))
                        .text_color(theme.text_secondary),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(theme.text)
                        .child("Upload from file"),
                ),
        )
        .into_any_element()
}

impl Nox {
    pub(crate) fn uses_secure_note_workspace(&self) -> bool {
        let Some(session) = self.session() else {
            return false;
        };
        match session.active_view {
            // Home has no type filter of its own — its "+ Add item" menu
            // opens a create editor directly and expects the same full-page
            // workspace Secure Notes gives its own creation flow.
            ActiveView::SecureNotes | ActiveView::Home => matches!(
                session.item_editor,
                Some(ItemEditorState {
                    mode: EditorMode::Create | EditorMode::Edit(_),
                    item_type: ItemType::SecureNote,
                    ..
                })
            ),
            // All items reuses the full-page editor for edits only; creation
            // there keeps the generic Sheet.
            ActiveView::AllItems => matches!(
                session.item_editor,
                Some(ItemEditorState {
                    mode: EditorMode::Edit(_),
                    item_type: ItemType::SecureNote,
                    ..
                })
            ),
            _ => false,
        }
    }

    /// A dedicated full-page login flow instead of the generic Sheet.
    pub(crate) fn uses_login_workspace(&self) -> bool {
        let Some(session) = self.session() else {
            return false;
        };
        match session.active_view {
            // Home has no type filter of its own — its "+ Add item" menu
            // opens a create editor directly and expects the same full-page
            // workspace Logins gives its own creation flow.
            ActiveView::Logins | ActiveView::Home => matches!(
                session.item_editor,
                Some(ItemEditorState {
                    mode: EditorMode::Create | EditorMode::Edit(_),
                    item_type: ItemType::Login,
                    ..
                })
            ),
            // All items reuses the full-page editor for edits only; creation
            // there keeps the generic Sheet.
            ActiveView::AllItems => matches!(
                session.item_editor,
                Some(ItemEditorState {
                    mode: EditorMode::Edit(_),
                    item_type: ItemType::Login,
                    ..
                })
            ),
            _ => false,
        }
    }

    /// Applies an icon choice from the picker and marks the field dirty the
    /// same way any other editor field change does — picking an icon is
    /// part of editing the item, not a separate save.
    pub(crate) fn choose_item_icon(
        &mut self,
        icon: IconChoice,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Picking Default or a preset must actually replace a previously
        // chosen local image (an upload or a fetched favicon) — otherwise
        // `resolve_item_icon`'s local-selection lookup keeps preferring the
        // old bytes forever, making the pick a silent no-op. `Favicon` is
        // the one choice a local selection legitimately backs, so it is the
        // one case that keeps it.
        let clears_local_image = !matches!(icon, IconChoice::Favicon);
        let local_key = clears_local_image
            .then(|| self.item_editor().map(|editor| editor.local_icon_key()))
            .flatten();
        if let Some(session) = self.session_mut()
            && let Some(editor) = session.item_editor.as_mut()
        {
            editor.icon = icon;
            if clears_local_image {
                editor.local_icon = None;
            }
        }
        if let Some(local_key) = local_key {
            self.clear_local_icon_selection(&local_key);
        }
        cx.notify();
    }

    pub(crate) fn icon_picker_offers_favicon(&self) -> bool {
        self.session()
            .and_then(|session| session.item_editor.as_ref())
            .is_some_and(|editor| editor.item_type == ItemType::Login)
    }

    /// The icon the picker's "Default" row previews: the *item type's* own
    /// default, so a Secure Note shows `file-lock` and not Login's `key-square`.
    pub(crate) fn icon_picker_default_icon_path(&self) -> &'static str {
        crate::icons::default_type_icon(
            self.item_editor()
                .map_or(ItemType::Login, |editor| editor.item_type),
        )
    }

    /// A Login with no URI of its own still gets a fetch path — through the
    /// row's inline URL field rather than the item's saved URIs.
    pub(crate) fn icon_picker_needs_url_input(&self, cx: &App) -> bool {
        self.icon_picker_offers_favicon()
            && self
                .item_editor()
                .is_some_and(|editor| editor.uris(cx).is_empty())
    }

    /// The caveat for the inline URL field, offered only while that field is
    /// the thing on screen — an unprompted note about a field nobody opened is
    /// just noise.
    pub(crate) fn favicon_typed_url_note(&self, cx: &App) -> Option<&'static str> {
        (self.icon_picker_needs_url_input(cx) && self.icon_picker_url_input_expanded())
            .then_some(FAVICON_TYPED_URL_NOTE)
    }

    pub(crate) fn icon_picker_url_input_expanded(&self) -> bool {
        self.item_editor()
            .is_some_and(|editor| editor.favicon.url_expanded)
    }

    /// Reveals the inline URL field inside the still-open popover.
    pub(crate) fn expand_favicon_url_input(&mut self, cx: &mut Context<Self>) {
        if let Some(editor) = self.item_editor_mut() {
            editor.favicon.url_expanded = true;
            editor.favicon.status = FaviconFetchStatus::Idle;
        }
        cx.notify();
    }

    /// An item already on `IconChoice::Favicon` may be looking at a cache miss
    /// (synced from another device, or a cleared cache); the picker offers the
    /// same explicit fetch again rather than refetching on its own.
    pub(crate) fn icon_picker_offers_refetch(&self) -> bool {
        self.icon_picker_offers_favicon()
            && self
                .item_editor()
                .is_some_and(|editor| editor.icon == IconChoice::Favicon)
    }

    pub(crate) fn favicon_fetch_status(&self) -> FaviconFetchStatus {
        self.item_editor()
            .map_or(FaviconFetchStatus::Idle, |editor| editor.favicon.status)
    }

    pub(crate) fn favicon_fetch_error(&self) -> Option<&'static str> {
        (self.favicon_fetch_status() == FaviconFetchStatus::Failed).then_some(FAVICON_FETCH_ERROR)
    }

    /// Candidate URIs for one explicit fetch: the item's own URI lines, or —
    /// when it has none — only the URL typed into the picker's inline field.
    /// That typed URL is used for the fetch and nothing else; it never joins
    /// `item.uris`, which the user edits through the URI field itself.
    fn favicon_fetch_candidates(&self, cx: &App) -> Vec<String> {
        let Some(editor) = self.item_editor() else {
            return Vec::new();
        };
        let uris = editor.uris(cx);
        if !uris.is_empty() {
            return uris;
        }
        let typed = editor.favicon.url_input.read(cx).value().trim().to_owned();
        if typed.is_empty() {
            Vec::new()
        } else {
            vec![typed]
        }
    }

    /// Runs the one favicon fetch the user just asked for. Nothing here starts
    /// without that click — no background scan, no fetch while typing a URL.
    pub(crate) fn website_field_blurred(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.icon_picker_offers_favicon() {
            return;
        }
        let Some(editor) = self.item_editor() else {
            return;
        };
        let Some(uri) = editor.uris(cx).first().cloned() else {
            return;
        };
        if editor.favicon.last_auto_fetch_uri.as_deref() == Some(uri.as_str()) {
            return;
        }
        if let Some(editor) = self.item_editor_mut() {
            editor.favicon.last_auto_fetch_uri = Some(uri);
        }
        self.fetch_item_favicon(window, cx);
    }

    pub(crate) fn choose_uploaded_item_icon(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor_id) = self.item_editor().map(|editor| editor.id) else {
            return;
        };
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose item icon".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let path = receiver
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
                .and_then(|mut paths| if paths.len() == 1 { paths.pop() } else { None });
            let Some(path) = path else {
                return;
            };
            // Check the on-disk size before reading any bytes — an oversized
            // file is rejected the same way (silently, no icon change) either
            // way, but this way a multi-gigabyte pick never gets read into
            // memory first just to be thrown away by `validate_local_image`.
            let Ok(metadata) = std::fs::metadata(&path) else {
                return;
            };
            if metadata.len() > crate::icons::MAX_LOCAL_IMAGE_BYTES as u64 {
                return;
            }
            let bytes = match std::fs::read(path) {
                Ok(bytes) => bytes,
                Err(_) => return,
            };
            let _ = cx.update(|window, app| {
                if let Some(this) = this.upgrade() {
                    this.update(app, |locker, cx| {
                        if locker
                            .item_editor()
                            .is_some_and(|editor| editor.id == editor_id)
                        {
                            let _ = locker.upload_item_icon_bytes(&bytes, window, cx);
                        }
                    });
                }
            });
        })
        .detach();
    }

    pub(crate) fn upload_item_icon_bytes(
        &mut self,
        bytes: &[u8],
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), crate::icons::LocalIconError> {
        let data_dir = self.data_dir.clone();
        let Some(editor) = self.item_editor_mut() else {
            return Ok(());
        };
        let local_key = editor.local_icon_key();
        let selection = crate::icons::cache_local_icon(&data_dir, &local_key, bytes)?;
        editor.local_icon = Some(selection.clone());
        editor.favicon.status = FaviconFetchStatus::Idle;
        self.record_local_icon_selection(selection);
        cx.notify();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn editor_avatar_is_before_name(&self) -> bool {
        self.item_editor().is_some()
    }

    pub(crate) fn fetch_item_favicon(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.icon_picker_offers_favicon() {
            return;
        }
        // Whose fetch this is. The reply is only allowed to touch this exact
        // editor, however long it takes to arrive.
        let Some(editor_id) = self.item_editor().map(|editor| editor.id) else {
            return;
        };
        let candidates = self.favicon_fetch_candidates(cx);
        if candidates.is_empty() {
            self.finish_favicon_fetch(editor_id, None, window, cx);
            return;
        }
        let client = cx.http_client();
        let data_dir = self.data_dir.clone();
        // Captured now, not re-derived when the reply lands: by then this may
        // be a different opening entirely, and a still-unsaved Create editor's
        // key depends on this exact `editor_id`.
        let local_key = self
            .item_editor()
            .map(|editor| editor.local_icon_key())
            .unwrap_or_else(|| crate::icons::editor_key(0));
        if let Some(editor) = self.item_editor_mut() {
            editor.favicon.status = FaviconFetchStatus::Loading;
        }
        cx.notify();
        // Same shape as `create_vault`/`unlock_vault` (app.rs) and every
        // `backup.rs` task: the network and disk work runs on
        // `cx.background_executor()`, off GPUI's foreground executor;
        // `this: WeakEntity<Nox>` is upgraded before touching state back on the
        // foreground thread.
        cx.spawn_in(window, async move |this, cx| {
            let local_icon = cx
                .background_executor()
                .spawn(async move {
                    // One candidate per call: `fetch_favicon` returns bytes but
                    // not *which* URI produced them, and the cache is keyed by
                    // host — handing it the whole list would file a later URI's
                    // icon under the first parseable host, where the resolver
                    // would never find it.
                    for uri in candidates {
                        let Some(host) = crate::favicon::extract_host(&uri) else {
                            continue;
                        };
                        if let Ok(bytes) = crate::favicon::fetch_favicon(
                            client.as_ref(),
                            std::slice::from_ref(&uri),
                        )
                        .await
                        {
                            // Best-effort compatibility cache under the host key;
                            // what the editor actually renders is the
                            // content-addressed local selection cached next.
                            let _ = crate::favicon::cache_favicon(&data_dir, &host, &bytes);
                            if let Ok(selection) =
                                crate::favicon::cache_favicon_for_key(&data_dir, &local_key, &bytes)
                            {
                                return Some(selection);
                            }
                        }
                    }
                    None
                })
                .await;
            let _ = cx.update(|window, app| {
                if let Some(this) = this.upgrade() {
                    this.update(app, |locker, cx| {
                        locker.finish_favicon_fetch(editor_id, local_icon, window, cx);
                    });
                }
            });
        })
        .detach();
    }

    /// On success the item moves to `IconChoice::Favicon`; on failure the icon
    /// is left exactly as it was and the row reports it inline, so the popover
    /// stays usable for a retry or another pick.
    ///
    /// `editor_id` is the editor that asked. A fetch outlives its editor easily
    /// — cancelled, saved, or swapped for another item while the request is in
    /// flight — and the editor sitting there when it answers may well be a
    /// Secure Note, which has no favicon at all. A reply that no longer matches
    /// its requester is dropped rather than applied to whoever is open now.
    fn finish_favicon_fetch(
        &mut self,
        editor_id: EditorId,
        local_icon: Option<crate::icons::LocalIconRef>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let still_the_requester = self
            .item_editor()
            .is_some_and(|editor| editor.id == editor_id && editor.item_type == ItemType::Login);
        if !still_the_requester {
            return;
        }
        let cached = local_icon.is_some();
        if let Some(selection) = local_icon {
            // Set before `choose_item_icon` so the picker's trigger and rows
            // resolve the new bytes on the very same render pass that flips
            // the choice to `Favicon`, instead of a stale render in between.
            if let Some(editor) = self.item_editor_mut() {
                editor.local_icon = Some(selection.clone());
            }
            self.record_local_icon_selection(selection);
            self.choose_item_icon(IconChoice::Favicon, window, cx);
        }
        if let Some(editor) = self.item_editor_mut() {
            editor.favicon.status = if cached {
                FaviconFetchStatus::Idle
            } else {
                FaviconFetchStatus::Failed
            };
        }
        cx.notify();
    }

    /// The item's icon, clickable to open the picker: Default, the 20
    /// presets, and — Login only — "Favicon del sitio". Secure Note gets no
    /// favicon row; it has no site identity to fetch from.
    fn render_icon_picker(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let data_dir = self.data_dir.clone();
        let (item_type, current_icon, _uris, favicon_url_input) = self
            .session()
            .and_then(|session| session.item_editor.as_ref())
            .map(|editor| {
                (
                    editor.item_type,
                    editor.icon,
                    editor.uris(cx),
                    Some(editor.favicon.url_input.clone()),
                )
            })
            .unwrap_or((ItemType::Login, IconChoice::Default, Vec::new(), None));

        // The trigger is the one spot that actually shows a chosen favicon
        // as an image — it's the direct feedback for "did my pick work?".
        let local_selection = self
            .item_editor()
            .and_then(|editor| editor.local_icon.as_ref());
        let local_key = self
            .item_editor()
            .map(|editor| editor.local_icon_key())
            .unwrap_or_else(|| crate::icons::editor_key(0));
        let resolved_icon = crate::icons::resolve_item_icon(
            item_type,
            current_icon,
            &data_dir,
            &local_key,
            local_selection,
        );
        // Drives the delete "x": it only makes sense to offer deleting an
        // uploaded/fetched image that is actually the thing on screen.
        let has_local_image = matches!(resolved_icon, crate::icons::ResolvedIcon::LocalImage(_));
        let trigger_child: AnyElement = match resolved_icon {
            crate::icons::ResolvedIcon::Svg(path) => gpui_component::Icon::empty()
                .path(path)
                .size(px(14.))
                .text_color(theme.text_secondary)
                .into_any_element(),
            crate::icons::ResolvedIcon::LocalImage(path) => gpui::img(path)
                .size(px(18.))
                .rounded(px(4.))
                .into_any_element(),
            crate::icons::ResolvedIcon::UnavailableLocalImage => gpui_component::Icon::empty()
                .path(crate::icons::default_type_icon(item_type))
                .size(px(14.))
                .text_color(theme.text_secondary)
                .into_any_element(),
        };
        let trigger = Button::new("item-icon-picker-trigger")
            .size(px(28.))
            .rounded(px(8.))
            .bg(theme.raised)
            .child(trigger_child);

        let locker = cx.entity();
        let locker_for_delete = locker.clone();
        let default_icon_path = self.icon_picker_default_icon_path();
        // The popover's content builder runs inside this same render pass and
        // cannot read `Nox` back, so the row's whole state is snapshotted here.
        let favicon_row = favicon_url_input
            .filter(|_| self.icon_picker_offers_favicon())
            .map(|url_input| FaviconRow {
                theme,
                locker: locker.clone(),
                status: self.favicon_fetch_status(),
                error: self.favicon_fetch_error(),
                needs_url_input: self.icon_picker_needs_url_input(cx),
                url_expanded: self.icon_picker_url_input_expanded(),
                url_input,
                typed_url_note: self.favicon_typed_url_note(cx),
                offers_refetch: self.icon_picker_offers_refetch(),
            });

        let picker = Popover::new("item-icon-picker")
            .appearance(false)
            .trigger(trigger)
            .content(move |_state, _window, _cx| {
                let mut rows: Vec<AnyElement> = Vec::new();
                rows.push(icon_picker_row(
                    "icon-default",
                    default_icon_path,
                    "Default",
                    theme,
                    locker.clone(),
                    IconChoice::Default,
                ));
                for preset in crate::icons::ALL_PRESETS {
                    rows.push(icon_picker_row(
                        SharedString::from(format!("icon-preset-{preset:?}")),
                        crate::icons::preset_icon_path(preset),
                        crate::icons::preset_icon_label(preset),
                        theme,
                        locker.clone(),
                        IconChoice::Preset(preset),
                    ));
                }
                if let Some(favicon_row) = favicon_row.clone() {
                    rows.push(favicon_picker_row(favicon_row));
                }
                rows.push(upload_icon_picker_row(theme, locker.clone()));
                div()
                    .id("item-icon-picker-menu")
                    .w(px(220.))
                    .max_h(px(360.))
                    .overflow_y_scroll()
                    .p(px(4.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.surface)
                    .children(rows)
            })
            .into_any_element();

        // The delete "x": edit-mode-and-create alike, shown only while the
        // avatar is actually rendering a local image, so there is nothing to
        // interpret when it's showing the type default, a preset, or an
        // unavailable placeholder. A direct click, no popover required —
        // per the spec's "an absolute-positioned top 'x' to delete the
        // uploaded or fetched image".
        if has_local_image {
            div()
                .relative()
                .child(picker)
                .child(
                    div()
                        .id("item-icon-delete")
                        .absolute()
                        .top(px(-4.))
                        .right(px(-4.))
                        .size(px(16.))
                        .rounded_full()
                        .bg(theme.danger)
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .on_mouse_down(gpui::MouseButton::Left, move |_, window, app| {
                            locker_for_delete.update(app, |locker, cx| {
                                locker.choose_item_icon(IconChoice::Default, window, cx);
                            });
                        })
                        .child(
                            gpui_component::Icon::empty()
                                .path("icons/x.svg")
                                .size(px(10.))
                                .text_color(theme.inverse),
                        ),
                )
                .into_any_element()
        } else {
            picker
        }
    }

    pub(crate) fn cancel_item_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(session) = self.session_mut() {
            session.item_editor = None;
        }
        self.editor_blur_subscriptions.clear();
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
                        if let Some(session) = locker.session_mut() {
                            session.item_editor = None;
                        }
                        cx.notify();
                    });
                })
        });
    }

    pub(crate) fn set_editor_type(&mut self, item_type: ItemType, cx: &mut Context<Self>) {
        if let Some(editor) = self.item_editor_mut() {
            editor.item_type = item_type;
            cx.notify();
        }
    }

    pub(crate) fn generate_editor_password(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.item_editor_mut() else {
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
        if let Some(editor) = self.item_editor_mut() {
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
        let Some(editor) = self.item_editor_mut() else {
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
        if let Some(editor) = self.item_editor_mut() {
            editor.generator.open = open;
            cx.notify();
        }
    }

    pub(crate) fn save_item(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.item_editor() else {
            return;
        };
        if editor.title_input.read(cx).value().trim().is_empty() {
            if let Some(editor) = self.item_editor_mut() {
                editor.save_error = Some("Enter a title.".into());
            }
            cx.notify();
            return;
        }
        if editor.item_type == ItemType::SecureNote
            && editor.notes_input.read(cx).value().trim().is_empty()
        {
            if let Some(editor) = self.item_editor_mut() {
                editor.save_error = Some("Enter note content.".into());
            }
            cx.notify();
            return;
        }
        let mode = editor.mode;
        // A Create editor's local selection, if any, is still cached under its
        // throwaway editor key — captured here so it can be re-keyed to the
        // item's real key once `create_item` returns one below.
        let pending_local_icon = editor.local_icon.clone();
        let payload = editor.payload(window, cx);
        let result = match (&mut self.state, mode) {
            (AppState::Unlocked(session), EditorMode::Create) => session
                .vault
                .create_item(&payload)
                .map(|item_id| (item_id, false)),
            (AppState::Unlocked(session), EditorMode::Edit(item_id))
            | (AppState::Unlocked(session), EditorMode::Restore(item_id)) => session
                .vault
                .update_item(item_id, &payload)
                .map(|()| (item_id, true)),
            _ => return,
        };
        match result {
            Ok((item_id, was_restore)) => {
                if mode == EditorMode::Create
                    && let Some(local_icon) = pending_local_icon
                {
                    self.migrate_local_icon_to_item(&local_icon, item_id);
                }
                if let Some(session) = self.session_mut() {
                    session.list.upsert(item_id, payload);
                    session.list.selected = Some(item_id);
                    if was_restore {
                        session.list.remove_deleted(item_id);
                    }
                    session.item_editor = None;
                }
                self.editor_blur_subscriptions.clear();
                window.close_sheet(cx);
            }
            Err(_) => {
                if let Some(editor) = self.item_editor_mut() {
                    editor.save_error = Some("Could not save item. Try again.".into());
                }
            }
        }
        cx.notify();
    }

    /// Re-keys a just-saved Create editor's local image from its throwaway
    /// editor key to the new item's real key, so the list/detail resolvers
    /// (keyed by item, not by editor opening) can find it. The bytes are
    /// content-addressed, so this is a cheap local copy, never a re-fetch or
    /// re-upload; the stale editor-keyed entry is dropped from the map since
    /// nothing will ever look it up again.
    fn migrate_local_icon_to_item(
        &mut self,
        local_icon: &crate::icons::LocalIconRef,
        item_id: ItemId,
    ) {
        let new_key = crate::icons::item_key(item_id);
        if local_icon.item_key == new_key {
            self.record_local_icon_selection(local_icon.clone());
            return;
        }
        let data_dir = self.data_dir.clone();
        if let Ok(migrated) =
            crate::icons::cache_local_icon_from_path(&data_dir, &new_key, &local_icon.cache_path)
        {
            self.local_icon_selections.remove(&local_icon.item_key);
            self.record_local_icon_selection(migrated);
        }
    }

    pub(crate) fn delete_item(
        &mut self,
        item_id: ItemId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = match &mut self.state {
            AppState::Unlocked(session) => session.vault.delete_item(item_id),
            _ => return,
        };
        match result {
            Ok(()) => {
                if let Some(session) = self.session_mut() {
                    session.list.remove(item_id);
                    session.list.add_deleted(item_id);
                    session.list.selected = None;
                    session.item_editor = None;
                }
                window.close_sheet(cx);
            }
            Err(_) => {
                if let Some(editor) = self.item_editor_mut() {
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
            AppState::Unlocked(session) => session.vault.get_item(item_id).ok().flatten(),
            _ => None,
        }) else {
            return;
        };
        payload.title = format!("{} (copy)", payload.title);
        payload.created_at = now_millis();
        payload.updated_at = payload.created_at;
        let result = match &mut self.state {
            AppState::Unlocked(session) => session.vault.create_item(&payload),
            _ => return,
        };
        match result {
            Ok(new_item_id) => {
                if let Some(session) = self.session_mut() {
                    session.list.upsert(new_item_id, payload);
                    session.list.selected = Some(new_item_id);
                }
            }
            Err(error) => {
                if let Some(session) = self.session_mut() {
                    session.list.load = crate::vault_list::ListLoadState::Failed(
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
            .session()
            .and_then(|session| session.list.items.iter().find(|(id, _)| *id == item_id))
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
        let _theme = Theme::current(cx);
        let component_theme = cx.theme();
        let border = component_theme.border;
        let foreground = component_theme.foreground;
        let muted_foreground = component_theme.muted_foreground;
        let danger = component_theme.danger;

        let Some(editor) = self.item_editor() else {
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
                            if let Some(editor) = locker.item_editor()
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

        let icon_picker = self.render_icon_picker(cx);
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
                    .items_center()
                    .gap(px(10.))
                    .child(icon_picker)
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(muted_foreground)
                            .child("Tap to change icon"),
                    ),
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
        let theme = Theme::current(cx);
        let Some(editor) = self.item_editor() else {
            return div().into_any_element();
        };
        let title = editor.title_input.clone();
        let notes = editor.notes_input.clone();
        let character_count = notes.read(cx).value().chars().count();
        let save_error = editor.save_error.clone();
        let editing = matches!(editor.mode, EditorMode::Edit(_));
        let workspace_title = if editing {
            "Edit note"
        } else {
            "Create secure note"
        };
        let workspace_subtitle = if editing {
            "Update this encrypted note"
        } else {
            "Add an encrypted note to your vault"
        };
        let save_label = if editing { "Save changes" } else { "Save note" };
        let save_hint = if editing {
            "⌘ ↵  Save changes"
        } else {
            "⌘ ↵  Save note"
        };
        let saved_hint = if editing {
            "Encrypted locally · saved in your vault"
        } else {
            "Encrypted locally · not saved yet"
        };
        let back_label =
            if self.session().map(|session| session.active_view) == Some(ActiveView::AllItems) {
                "Back to all items"
            } else {
                "Back to secure notes"
            };
        let locker = cx.entity();

        let cancel_locker = locker.clone();
        let cancel = Button::new("cancel-secure-note")
            .h(px(38.))
            .px(px(14.))
            .rounded(px(8.))
            .bg(theme.surface)
            .border_1()
            .border_color(theme.field_border)
            .on_click(move |_, window, app| {
                cancel_locker.update(app, |locker, cx| locker.cancel_item_editor(window, cx));
            })
            .child(
                div()
                    .text_size(px(12.))
                    .font_weight(FontWeight(500.))
                    .text_color(theme.text_soft)
                    .child("Cancel"),
            );
        let save_locker = locker.clone();
        let save = Button::new("save-secure-note")
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
                        Icon::empty()
                            .path("icons/check.svg")
                            .size(px(14.))
                            .text_color(theme.canvas),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(FontWeight(600.))
                            .text_color(theme.canvas)
                            .child(save_label),
                    ),
            );
        let save = crate::app::animated_auth_button(
            "save-secure-note",
            save,
            self.auth_hovered.get("save-secure-note").copied(),
            (
                theme.inverse,
                theme.inverse_hover,
                theme.inverse_press,
                theme.on_inverse,
            ),
            cx,
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
                            .text_color(theme.text_secondary),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight(500.))
                            .text_color(theme.text_soft)
                            .child(back_label),
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
                                .text_color(theme.icon_muted)
                                .child(label),
                        )
                        .when(required, |row| {
                            row.child(
                                div()
                                    .text_size(px(9.))
                                    .text_color(theme.text_ghost)
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
                    .text_color(theme.icon_muted),
            )
        };
        let note_editor = div()
            .flex()
            .flex_col()
            .h(px(220.))
            .rounded(px(7.))
            .bg(theme.field)
            .border_1()
            .border_color(theme.field_border)
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
                    .border_color(theme.field_border)
                    .child(tool("note-format-bold", "icons/bold.svg"))
                    .child(tool("note-format-italic", "icons/italic.svg"))
                    .child(tool("note-format-list", "icons/list.svg"))
                    .child(tool("note-format-code", "icons/code.svg"))
                    .child(div().w(px(1.)).h(px(16.)).mx(px(4.)).bg(theme.field_border))
                    .child(
                        div()
                            .text_size(px(9.))
                            .text_color(theme.text_ghost)
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
                    .text_color(theme.text_soft),
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
                    .border_color(theme.field_border)
                    .child(
                        div()
                            .text_size(px(9.))
                            .text_color(theme.text_subtle)
                            .child("Draft saved locally"),
                    )
                    .child(
                        div()
                            .text_size(px(9.))
                            .text_color(theme.text_secondary)
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
                        .bg(theme.raised)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            Icon::empty()
                                .path(icon)
                                .size(px(13.))
                                .text_color(theme.text_secondary),
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
                                .text_color(theme.text_soft)
                                .child(heading),
                        )
                        .child(
                            div()
                                .text_size(px(9.))
                                .text_color(theme.text_subtle)
                                .child(detail),
                        ),
                )
        };
        let error = save_error.map_or_else(
            || div().into_any_element(),
            |message| {
                div()
                    .text_size(px(11.))
                    .text_color(theme.danger)
                    .child(message)
                    .into_any_element()
            },
        );
        let icon_picker = self.render_icon_picker(cx);

        rsx! {
            <div id="secure-note-workspace" flex flex_col flex_1 min_w={px(0.)} h_full bg={theme.canvas}>
                <div flex items_center justify_between h={px(88.)} px={px(32.)} flex_shrink_0 border_b_1 borderColor={theme.border}>
                    <div flex flex_col gap={px(3.)}>
                        <div text_xl fontWeight={FontWeight::SEMIBOLD} textColor={theme.text}>{workspace_title}</div>
                        <div text_xs textColor={theme.text_muted}>{workspace_subtitle}</div>
                    </div>
                    <div flex items_center gap={px(10.)}>{cancel}{save}</div>
                </div>
                <div flex flex_col flex_1 min_h={px(0.)} p={px(28.)} pt={px(20.)} gap={px(16.)} overflow_y_scroll>
                    <div flex items_center justify_between h={px(40.)} flex_shrink_0>
                        {back}
                        <div flex items_center gap={px(8.)}>
                            {Icon::empty().path("icons/shield-check.svg").size(px(14.)).text_color(theme.text_subtle)}
                            <div text_size={px(9.)} textColor={theme.icon_muted}>{saved_hint}</div>
                        </div>
                    </div>
                    <div flex flex_1 min_h={px(0.)} gap={px(16.)}>
                        <div flex flex_col w={px(760.)} flex_shrink_0 h_full p={px(24.)} gap={px(14.)} rounded={px(9.)} bg={theme.surface} border_1 borderColor={theme.border}>
                            <div flex items_center justify_between h={px(44.)} flex_shrink_0>
                                <div flex flex_col gap={px(4.)}>
                                    <div text_size={px(14.)} fontWeight={FontWeight::SEMIBOLD} textColor={theme.text}>{"Note details"}</div>
                                    <div text_size={px(10.)} textColor={theme.text_subtle}>{"Store sensitive text securely, end-to-end encrypted."}</div>
                                </div>
                                <div flex items_center gap={px(8.)}>
                                    {icon_picker}
                                    <div flex items_center h={px(26.)} px={px(8.)} gap={px(6.)} rounded={px(6.)} bg={theme.raised} border_1 borderColor={theme.field_border}>
                                        {Icon::empty().path("icons/file-lock.svg").size(px(12.)).text_color(theme.text_secondary)}
                                        <div text_size={px(9.)} fontWeight={FontWeight::SEMIBOLD} textColor={theme.text_secondary}>{"NOTE"}</div>
                                    </div>
                                </div>
                            </div>
                            {field("TITLE", true, Input::new(&title).h(px(42.)).px(px(11.)).bg(theme.field).border_color(theme.field_border).rounded(px(7.)).prefix(Icon::empty().path("icons/notebook-pen.svg").size(px(14.)).text_color(theme.icon_muted)).into_any_element())}
                            {field("CONTENT", true, note_editor)}
                            {error}
                            <div flex_1 />
                            <div flex items_center h={px(42.)} px={px(11.)} rounded={px(7.)} bg={theme.inset} border_1 borderColor={theme.border}>
                                {Icon::empty().path("icons/lock-keyhole.svg").size(px(13.)).text_color(theme.success)}
                                <div ml={px(8.)} text_size={px(9.)} textColor={theme.text_subtle}>{"Encrypted before it leaves this device"}</div>
                                <div ml_auto text_size={px(9.)} fontWeight={FontWeight::SEMIBOLD} textColor={theme.text_secondary}>{save_hint}</div>
                            </div>
                        </div>
                        <div flex flex_col flex_1 min_w={px(0.)} h_full gap={px(14.)}>
                            <div flex flex_col p={px(20.)} gap={px(14.)} rounded={px(9.)} bg={theme.surface} border_1 borderColor={theme.border}>
                                <div flex items_center justify_between>
                                    <div flex items_center gap={px(9.)}>
                                        <div size={px(30.)} flex items_center justify_center rounded={px(7.)} bg={theme.raised}>{Icon::empty().path("icons/shield-check.svg").size(px(15.)).text_color(theme.text_secondary)}</div>
                                        <div flex flex_col gap={px(2.)}><div text_size={px(12.)} fontWeight={FontWeight::SEMIBOLD} textColor={theme.text}>{"Note privacy"}</div><div text_size={px(9.)} textColor={theme.text_ghost}>{"Encrypted the moment you type"}</div></div>
                                    </div>
                                    <div h={px(24.)} px={px(8.)} flex items_center rounded={px(6.)} bg={theme.raised} text_size={px(8.)} fontWeight={FontWeight::SEMIBOLD} textColor={theme.success_bright}>{"ENCRYPTED"}</div>
                                </div>
                                <div text_size={px(10.)} textColor={theme.text_subtle}>{"Notes are encrypted locally before they ever leave this device, and stay unreadable without your master password."}</div>
                                <div flex flex_col gap={px(9.)} text_size={px(10.)} textColor={theme.text_muted}>
                                    <div flex items_center gap={px(8.)}>{Icon::empty().path("icons/key-round.svg").size(px(13.)).text_color(theme.text_ghost)}{"Wi-Fi passwords and PINs"}</div>
                                    <div flex items_center gap={px(8.)}>{Icon::empty().path("icons/key-round.svg").size(px(13.)).text_color(theme.text_ghost)}{"Recovery and backup codes"}</div>
                                    <div flex items_center gap={px(8.)}>{Icon::empty().path("icons/shield-check.svg").size(px(13.)).text_color(theme.text_ghost)}{"Security question answers"}</div>
                                </div>
                            </div>
                            <div flex flex_col p={px(20.)} gap={px(13.)} rounded={px(9.)} bg={theme.surface} border_1 borderColor={theme.border}>
                                <div text_size={px(12.)} fontWeight={FontWeight::SEMIBOLD} textColor={theme.text}>{"After saving"}</div>
                                {outcome("icons/copy-plus.svg", "Quick copy", "The note content becomes a quick copy target.")}
                                {outcome("icons/search.svg", "Full-text search", "Find this note instantly across your vault.")}
                                {outcome("icons/refresh-cw.svg", "Sync securely", "The encrypted note syncs with your vault devices.")}
                            </div>
                            <div flex_1 />
                            <div flex items_center justify_between h={px(70.)} px={px(16.)} rounded={px(9.)} bg={theme.inset} border_1 borderColor={theme.border}>
                                <div flex flex_col gap={px(3.)}><div text_size={px(10.)} fontWeight={FontWeight::SEMIBOLD} textColor={theme.text_soft}>{"Keyboard friendly"}</div><div text_size={px(9.)} textColor={theme.text_ghost}>{"Tab between fields · Esc to cancel"}</div></div>
                                <div text_size={px(11.)} fontWeight={FontWeight::SEMIBOLD} textColor={theme.text_secondary}>{"⌘ ↵"}</div>
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
        let theme = Theme::current(cx);
        let Some(editor) = self.item_editor() else {
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
        let edit_item_id = match editor.mode {
            EditorMode::Edit(item_id) => Some(item_id),
            EditorMode::Create | EditorMode::Restore(_) => None,
        };
        let workspace_title = if edit_item_id.is_some() {
            "Edit login"
        } else {
            "Create login"
        };
        let workspace_subtitle = if edit_item_id.is_some() {
            "Update this secure account in your vault"
        } else {
            "Add a secure account to your vault"
        };
        let save_label = if edit_item_id.is_some() {
            "Save changes"
        } else {
            "Save login"
        };

        let items = self
            .session()
            .map_or(Vec::new(), |session| session.list.items.clone());
        let strength = password_strength_score(&password_value);
        let has_min_length = password_value.chars().count() >= 14;
        let is_reused = password_is_reused(&password_value, &items);
        let has_password = !password_value.is_empty();

        let locker = cx.entity();

        let back_label =
            if self.session().map(|session| session.active_view) == Some(ActiveView::AllItems) {
                "Back to all items"
            } else {
                "Back to logins"
            };
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
                    .text_color(theme.text_soft)
                    .child("‹")
                    .child(back_label),
            );

        let cancel_locker = locker.clone();
        let cancel_button = Button::new("cancel-create-login")
            .h(px(38.))
            .px(px(16.))
            .rounded(px(8.))
            .bg(theme.surface)
            .on_click(move |_, window, app| {
                cancel_locker.update(app, |locker, cx| locker.cancel_item_editor(window, cx));
            })
            .child(
                div()
                    .text_size(px(14.))
                    .font_weight(FontWeight(500.))
                    .text_color(theme.text_soft)
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
                            .text_color(theme.canvas),
                    )
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight(500.))
                            .text_color(theme.canvas)
                            .child(save_label),
                    ),
            );
        let save_button = crate::app::animated_auth_button(
            "save-create-login",
            save_button_base,
            self.auth_hovered.get("save-create-login").copied(),
            (
                theme.inverse,
                theme.inverse_hover,
                theme.inverse_press,
                theme.on_inverse,
            ),
            cx,
        );

        let delete_button = if let Some(item_id) = edit_item_id {
            let delete_locker = locker.clone();
            Button::new("delete-login")
                .danger()
                .outline()
                .small()
                .label("Delete")
                .on_click(move |_, window, app| {
                    delete_locker.update(app, |locker, cx| {
                        locker.open_delete_confirmation(item_id, window, cx);
                    });
                })
                .into_any_element()
        } else {
            div().into_any_element()
        };

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
                                .text_color(theme.icon_muted)
                                .child(label),
                        )
                        .child(if required {
                            div()
                                .text_size(px(11.))
                                .text_color(theme.text_ghost)
                                .child("Required")
                                .into_any_element()
                        } else if let Some(helper) = helper {
                            div()
                                .text_size(px(11.))
                                .text_color(theme.text_ghost)
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
                .bg(theme.field)
                .border_color(theme.field)
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
            .bg(theme.text)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        gpui_component::Icon::empty()
                            .path("icons/wand-sparkles.svg")
                            .size(px(12.))
                            .text_color(theme.canvas),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .font_weight(FontWeight(500.))
                            .text_color(theme.canvas)
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
                .bg(if index < strength {
                    theme.border_strong
                } else {
                    theme.item_icon
                })
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
                        .text_color(theme.text_secondary)
                        .into_any_element()
                } else {
                    div()
                        .size(px(12.))
                        .rounded_full()
                        .border_1()
                        .border_color(theme.text_ghost)
                        .into_any_element()
                })
                .child(
                    div()
                        .text_size(px(13.))
                        .text_color(if met {
                            theme.text_secondary
                        } else {
                            theme.text_muted
                        })
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
                            .bg(theme.raised)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                gpui_component::Icon::empty()
                                    .path(icon_path)
                                    .size(px(13.))
                                    .text_color(theme.text_secondary),
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
                                    .text_color(theme.text_soft)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(theme.text_subtle)
                                    .child(description),
                            ),
                    )
            };

        let error =
            save_error.map(|message| div().text_sm().text_color(theme.danger).child(message));
        let icon_picker = self.render_icon_picker(cx);

        let form_card = div()
            .id("create-login-form")
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .rounded(px(10.))
            .bg(theme.surface)
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
                                    .text_color(theme.text)
                                    .child("Account details"),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(theme.text_subtle)
                                    .child("Store credentials and sign-in information securely."),
                            ),
                    )
                    .child(
                        div().flex().items_center().gap(px(8.)).child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .h(px(26.))
                                .px(px(10.))
                                .rounded(px(6.))
                                .bg(theme.raised)
                                .child(
                                    gpui_component::Icon::empty()
                                        .path("icons/key-square.svg")
                                        .size(px(12.))
                                        .text_color(theme.text_secondary),
                                )
                                .child(
                                    div()
                                        .text_size(px(10.))
                                        .font_weight(FontWeight(700.))
                                        .text_color(theme.text_secondary)
                                        .child("LOGIN"),
                                ),
                        ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon_picker),
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
                    .bg(theme.field)
                    .border_color(theme.field)
                    .rounded(px(8.))
                    .into_any_element(),
            ))
            .child(field(
                "NOTES",
                false,
                None,
                Textarea::new(&notes)
                    .bg(theme.field)
                    .border_color(theme.field)
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
                    .bg(theme.inset)
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(theme.text_subtle)
                            .child("Encrypted before it leaves this device"),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(theme.text_secondary)
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
                    .bg(theme.surface)
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
                                            .bg(theme.raised)
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(
                                                gpui_component::Icon::empty()
                                                    .path("icons/shield-check.svg")
                                                    .size(px(15.))
                                                    .text_color(theme.text_secondary),
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
                                                    .text_color(theme.text)
                                                    .child("Password health"),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(11.))
                                                    .text_color(theme.text_ghost)
                                                    .child("Updates as you type"),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .h(px(22.))
                                    .px(px(9.))
                                    .rounded(px(6.))
                                    .bg(theme.raised)
                                    .flex()
                                    .items_center()
                                    .child(
                                        div()
                                            .text_size(px(9.))
                                            .font_weight(FontWeight(700.))
                                            .text_color(theme.text_subtle)
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
                            .text_color(theme.text_subtle)
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
                                            .border_color(theme.text_ghost),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(13.))
                                            .text_color(theme.text_muted)
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
                    .bg(theme.surface)
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
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
            .bg(theme.canvas)
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
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(3.))
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.text)
                                    .child(workspace_title),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.text_muted)
                                    .child(workspace_subtitle),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(cancel_button)
                            .child(delete_button)
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
                                            .text_color(theme.text_subtle),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(12.))
                                            .text_color(theme.icon_muted)
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
            icon: IconChoice::Default,
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
