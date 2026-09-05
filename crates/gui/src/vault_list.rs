//! The "All items" / "Logins" / "Secure notes" split view: a real-time
//! filterable, searchable, sortable list of vault items on the left and the
//! selected item's detail panel (`detail.rs`) on the right. Matches the
//! Pencil "Nox — All Items · No Selection" frame.

use crate::app::{AppState, Nox};
use crate::nav::ActiveView;
use crate::theme::Theme;
use gpui::{
    AnyElement, Context, ElementId, Entity, Focusable, FontWeight, Hsla, KeyDownEvent,
    SharedString, Subscription, UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use gpui_component::{
    Disableable, Sizable,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
};
use nox_core::{ItemId, ItemPayload, ItemType, VaultError};
use std::collections::HashSet;

use crate::assets::{IconName, icon};

/// Colors from the Pencil "All Items Split Workspace" frame that don't
/// Column widths shared by the list header row and every item row, so the
/// two stay aligned.
const COL_TYPE_W: f32 = 110.;
const COL_UPDATED_W: f32 = 120.;
const COL_ACTIONS_W: f32 = 64.;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ListLoadState {
    Ready,
    Failed(SharedString),
}

/// How the item list is ordered. Both are real orderings over real fields —
/// there's no fabricated "relevance" sort.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SortMode {
    Updated,
    Name,
}

impl SortMode {
    fn label(self) -> &'static str {
        match self {
            SortMode::Updated => "Updated",
            SortMode::Name => "Name",
        }
    }

    fn toggled(self) -> Self {
        match self {
            SortMode::Updated => SortMode::Name,
            SortMode::Name => SortMode::Updated,
        }
    }
}

/// A login's password health, computed live from the plaintext passwords
/// already held in memory once the vault is unlocked — no new crypto or
/// storage, just two real (if simple) heuristics. There's no "Personal"/
/// "Work" equivalent in the Pencil "Login Smart Filters" row because there's
/// no tagging concept in the data model to honestly back it with.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LoginHealth {
    Weak,
    Reused,
}

/// Under 8 characters. A crude length-only heuristic, not a real entropy
/// estimate — upgrade to something like zxcvbn if that's ever asked for.
fn is_weak_password(password: &str) -> bool {
    !password.is_empty() && password.chars().count() < 8
}

/// Every password (non-empty) shared by more than one login.
pub(crate) fn duplicate_passwords(items: &[(ItemId, ItemPayload)]) -> HashSet<String> {
    let mut seen = HashSet::new();
    let mut dupes = HashSet::new();
    for (_, payload) in items {
        if payload.item_type != ItemType::Login || payload.password.is_empty() {
            continue;
        }
        if !seen.insert(payload.password.clone()) {
            dupes.insert(payload.password.clone());
        }
    }
    dupes
}

/// `(label, color)` for a login's password health, or `("—", muted)` when it
/// has no password set yet.
pub(crate) fn login_health(
    theme: Theme,
    payload: &ItemPayload,
    dupes: &HashSet<String>,
) -> (&'static str, Hsla) {
    if payload.password.is_empty() {
        return ("—", theme.text_subtle);
    }
    if dupes.contains(&payload.password) {
        ("Reused", theme.danger)
    } else if is_weak_password(&payload.password) {
        ("Weak", theme.danger)
    } else {
        ("Strong", theme.text_secondary)
    }
}

pub(crate) struct VaultListState {
    pub(crate) load: ListLoadState,
    pub(crate) items: Vec<(ItemId, ItemPayload)>,
    pub(crate) filtered: Vec<usize>,
    pub(crate) deleted_ids: Vec<ItemId>,
    pub(crate) search_input: Entity<InputState>,
    pub(crate) search_query_lower: String,
    pub(crate) selected: Option<ItemId>,
    pub(crate) deleted_expanded: bool,
    pub(crate) scroll_handle: UniformListScrollHandle,
    /// Optional item type shown by the active sidebar view; `None` means all items.
    pub(crate) type_filter: Option<ItemType>,
    pub(crate) sort_mode: SortMode,
    /// Only meaningful alongside `type_filter == Some(ItemType::Login)` — the
    /// Logins view's "Weak"/"Reused" smart filters.
    pub(crate) login_health_filter: Option<LoginHealth>,
    pub(crate) _search_subscription: Subscription,
}

impl VaultListState {
    pub(crate) fn from_initial_load(
        items: Result<Vec<(ItemId, ItemPayload)>, VaultError>,
        deleted_ids: Result<Vec<ItemId>, VaultError>,
        window: &mut Window,
        cx: &mut Context<Nox>,
    ) -> Self {
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let (load, items, deleted_ids) = match (items, deleted_ids) {
            (Ok(items), Ok(deleted_ids)) => (ListLoadState::Ready, items, deleted_ids),
            (Err(error), _) | (_, Err(error)) => (
                ListLoadState::Failed(format!("Could not load vault items: {error}").into()),
                Vec::new(),
                Vec::new(),
            ),
        };
        let _search_subscription =
            cx.subscribe(&search_input, |locker, _input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    locker.recompute_vault_list_filter(cx);
                }
            });
        let mut state = Self {
            load,
            items,
            filtered: Vec::new(),
            deleted_ids,
            search_input,
            search_query_lower: String::new(),
            selected: None,
            deleted_expanded: false,
            scroll_handle: UniformListScrollHandle::new(),
            type_filter: None,
            sort_mode: SortMode::Updated,
            login_health_filter: None,
            _search_subscription,
        };
        state.rebuild_filter();
        state
    }

    /// Switch which item type the list shows (or show all items),
    /// clearing the current selection since it likely belongs to the other type.
    pub(crate) fn set_type_filter(&mut self, item_type: Option<ItemType>) {
        if self.type_filter == item_type && self.login_health_filter.is_none() {
            return;
        }
        self.type_filter = item_type;
        self.login_health_filter = None;
        self.selected = None;
        self.rebuild_filter();
    }

    pub(crate) fn set_login_health_filter(&mut self, filter: Option<LoginHealth>) {
        if self.login_health_filter == filter {
            return;
        }
        self.login_health_filter = filter;
        self.selected = None;
        self.rebuild_filter();
    }

    pub(crate) fn toggle_sort_mode(&mut self) {
        self.sort_mode = self.sort_mode.toggled();
        self.rebuild_filter();
    }

    pub(crate) fn weak_login_count(&self) -> usize {
        self.items
            .iter()
            .filter(|(_, payload)| {
                payload.item_type == ItemType::Login && is_weak_password(&payload.password)
            })
            .count()
    }

    pub(crate) fn reused_login_count(&self) -> usize {
        let dupes = duplicate_passwords(&self.items);
        self.items
            .iter()
            .filter(|(_, payload)| {
                payload.item_type == ItemType::Login && dupes.contains(&payload.password)
            })
            .count()
    }

    pub(crate) fn recompute_filter(&mut self, query: String) {
        if self.search_query_lower == query {
            return;
        }
        self.search_query_lower = query;
        self.rebuild_filter();
    }

    fn rebuild_filter(&mut self) {
        let dupes = duplicate_passwords(&self.items);
        let mut filtered: Vec<usize> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, (_, payload))| {
                self.type_filter
                    .is_none_or(|item_type| payload.item_type == item_type)
                    && self.login_health_filter.is_none_or(|health| {
                        payload.item_type == ItemType::Login
                            && match health {
                                LoginHealth::Weak => is_weak_password(&payload.password),
                                LoginHealth::Reused => dupes.contains(&payload.password),
                            }
                    })
                    && (payload
                        .title
                        .to_lowercase()
                        .contains(&self.search_query_lower)
                        || payload
                            .username
                            .to_lowercase()
                            .contains(&self.search_query_lower)
                        || (payload.item_type == ItemType::SecureNote
                            && payload
                                .notes
                                .to_lowercase()
                                .contains(&self.search_query_lower)))
            })
            .map(|(index, _)| index)
            .collect();
        match self.sort_mode {
            SortMode::Updated => {
                filtered
                    .sort_by(|&a, &b| self.items[b].1.updated_at.cmp(&self.items[a].1.updated_at));
            }
            SortMode::Name => {
                filtered.sort_by(|&a, &b| {
                    self.items[a]
                        .1
                        .title
                        .to_lowercase()
                        .cmp(&self.items[b].1.title.to_lowercase())
                });
            }
        }
        self.filtered = filtered;
    }

    pub(crate) fn upsert(&mut self, item_id: ItemId, payload: ItemPayload) {
        if let Some((_, existing)) = self.items.iter_mut().find(|(id, _)| *id == item_id) {
            *existing = payload;
        } else {
            self.items.push((item_id, payload));
        }
        self.rebuild_filter();
    }

    pub(crate) fn remove(&mut self, item_id: ItemId) {
        self.items.retain(|(id, _)| *id != item_id);
        self.rebuild_filter();
    }

    pub(crate) fn add_deleted(&mut self, item_id: ItemId) {
        if !self.deleted_ids.contains(&item_id) {
            self.deleted_ids.push(item_id);
        }
    }

    pub(crate) fn remove_deleted(&mut self, item_id: ItemId) {
        self.deleted_ids.retain(|id| *id != item_id);
    }

    pub(crate) fn toggle_deleted(&mut self) {
        self.deleted_expanded = !self.deleted_expanded;
    }

    fn selected_index(&self) -> Option<usize> {
        self.selected.and_then(|selected| {
            self.filtered
                .iter()
                .position(|index| self.items[*index].0 == selected)
        })
    }

    fn type_count(&self, item_type: ItemType) -> usize {
        self.items
            .iter()
            .filter(|(_, payload)| payload.item_type == item_type)
            .count()
    }
}

/// The login's site host if it has one, the secure note's first line, or a
/// bare type name — all real fields, no placeholder text pretending to be data.
fn login_website_host(payload: &ItemPayload) -> Option<String> {
    let uri = payload.uris.first()?;
    let host = uri
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .filter(|host| !host.is_empty())?;
    Some(host.to_owned())
}

fn row_subtitle(payload: &ItemPayload) -> String {
    match payload.item_type {
        ItemType::Login => login_website_host(payload).unwrap_or_else(|| {
            if payload.username.is_empty() {
                "—".to_owned()
            } else {
                payload.username.clone()
            }
        }),
        ItemType::SecureNote => payload
            .notes
            .lines()
            .find(|line| !line.trim().is_empty())
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .unwrap_or("No additional details")
            .to_owned(),
    }
}

/// The Logins view's row subtitle: "username · website" (Pencil "Login
/// Subtitle" nodes), falling back gracefully when either half is missing.
fn login_row_subtitle(payload: &ItemPayload) -> String {
    let host = login_website_host(payload);
    match (payload.username.is_empty(), host) {
        (false, Some(host)) => format!("{} · {host}", payload.username),
        (false, None) => payload.username.clone(),
        (true, Some(host)) => host,
        (true, None) => "—".to_owned(),
    }
}

type PillClick = Box<dyn Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static>;

/// A type-filter pill in the "All Items Type Filters" row.
///
/// A builder rather than a wide function: of the nine call sites, seven want
/// the default colour and seven are enabled, so a flat signature made every
/// call spell out parameters it did not care about. It also matches how this
/// file already builds its `Button`s.
///
/// `count` is always the real total for that type in the vault (unaffected by
/// search), matching the design's counts.
struct TypeFilterPill {
    id: &'static str,
    label: &'static str,
    count: usize,
    count_color: Option<Hsla>,
    active: bool,
    enabled: bool,
    on_click: Option<PillClick>,
}

impl TypeFilterPill {
    fn new(id: &'static str, label: &'static str, count: usize) -> Self {
        Self {
            id,
            label,
            count,
            count_color: None,
            active: false,
            enabled: true,
            on_click: None,
        }
    }

    /// A filter for an item type this app does not model yet.
    ///
    /// Named because the three things it implies — a zero count, a disabled
    /// pill, and no click handler — always travel together and mean one thing.
    /// Shown at a real 0 rather than hidden, the same convention the sidebar
    /// uses.
    fn unavailable(id: &'static str, label: &'static str) -> Self {
        Self {
            enabled: false,
            ..Self::new(id, label, 0)
        }
    }

    fn count_color(mut self, color: Hsla) -> Self {
        self.count_color = Some(color);
        self
    }

    fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    fn on_click(
        mut self,
        handler: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }

    fn render(self, cx: &mut Context<Nox>) -> AnyElement {
        let theme = Theme::current(cx);
        let label_color = if self.active {
            theme.text
        } else {
            theme.text_muted
        };
        let content = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(
                div()
                    .text_size(px(13.))
                    .font_weight(FontWeight(500.))
                    .text_color(label_color)
                    .child(self.label),
            )
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(self.count_color.unwrap_or(theme.text_count))
                    .child(format!("{}", self.count)),
            );
        let (bg, hover_bg) = if self.active {
            (theme.pill_active, theme.pill_active)
        } else {
            (theme.surface, theme.raised)
        };
        let variant = ButtonCustomVariant::new(cx).color(bg).hover(hover_bg);
        Button::new(self.id)
            .disabled(!self.enabled)
            .custom(variant)
            .h(px(28.))
            .px(px(9.))
            .rounded(px(6.))
            .when_some(self.on_click, |button, handler| button.on_click(handler))
            .child(content)
            .into_any_element()
    }
}

/// One item row's inner content: icon, title/subtitle, type, updated time,
/// and a decorative "more" glyph (Edit/Delete already live one click away in
/// the detail panel once selected, so this isn't wired to a second menu).
///
/// `w_full()` neutralizes `Button`'s own hardcoded `justify_center` on
/// whichever button wraps this — see `nav.rs::sidebar_link` for the full
/// explanation of that trick.
#[allow(clippy::too_many_arguments)]
fn item_row_content(
    theme: Theme,
    icon: AnyElement,
    title: String,
    subtitle: String,
    third_column: (String, Hsla),
    updated: String,
    selected: bool,
) -> AnyElement {
    let (third_label, third_color) = third_column;
    // Matches the Pencil frame's selected-row treatment: the icon box
    // brightens along with the row itself, not just the row background.
    let icon_bg = if selected {
        theme.item_icon_selected
    } else {
        theme.item_icon
    };
    div()
        .flex()
        .items_center()
        .w_full()
        .gap(px(14.))
        .child(
            div()
                .size(px(32.))
                .flex_shrink_0()
                .rounded(px(8.))
                .bg(icon_bg)
                .flex()
                .items_center()
                .justify_center()
                .child(icon),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(0.))
                .gap(px(2.))
                .overflow_hidden()
                .child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text)
                        .truncate()
                        .child(title),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme.text_subtle)
                        .truncate()
                        .child(subtitle),
                ),
        )
        .child(
            div()
                .w(px(COL_TYPE_W))
                .flex_shrink_0()
                .text_size(px(12.))
                .text_color(third_color)
                .child(third_label),
        )
        .child(
            div()
                .w(px(COL_UPDATED_W))
                .flex_shrink_0()
                .text_size(px(12.))
                .text_color(theme.text_subtle)
                .child(updated),
        )
        .child(
            div()
                .w(px(COL_ACTIONS_W))
                .flex_shrink_0()
                .flex()
                .justify_end()
                .child(
                    gpui_component::Icon::empty()
                        .path("icons/ellipsis.svg")
                        .size(px(14.))
                        .text_color(theme.text_count),
                ),
        )
        .into_any_element()
}

/// The list header row's column labels — same widths as `item_row_content`
/// so header and rows stay aligned. Logins swaps "ITEM"/"TYPE" for
/// "ACCOUNT"/"HEALTH", matching the Pencil "Logins List Header" frame.
fn list_header_row(
    theme: Theme,
    first_column: &'static str,
    third_column: &'static str,
) -> AnyElement {
    let label = |text: &'static str| {
        div()
            .text_size(px(10.))
            .font_weight(FontWeight(600.))
            .text_color(theme.column_header)
            .child(text)
    };
    div()
        .flex()
        .items_center()
        .w_full()
        .h(px(34.))
        .px(px(14.))
        .gap(px(14.))
        .flex_shrink_0()
        .bg(theme.inset)
        .child(div().flex_1().min_w(px(0.)).child(label(first_column)))
        .child(
            div()
                .w(px(COL_TYPE_W))
                .flex_shrink_0()
                .child(label(third_column)),
        )
        .child(
            div()
                .w(px(COL_UPDATED_W))
                .flex_shrink_0()
                .child(label("UPDATED")),
        )
        .child(div().w(px(COL_ACTIONS_W)).flex_shrink_0())
        .into_any_element()
}

impl Nox {
    pub(crate) fn recompute_vault_list_filter(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session_mut() else {
            return;
        };
        let list = &mut session.list;
        let query = list.search_input.read(cx).value().to_lowercase();
        if list.search_query_lower == query {
            return;
        }
        list.recompute_filter(query);
        cx.notify();
    }

    pub(crate) fn register_website_blur_listener(
        &mut self,
        uris_focus: gpui::FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.entity();
        let subscription = window.on_focus_out(&uris_focus, cx, move |_event, window, app| {
            this.update(app, |locker, cx| {
                locker.website_field_blurred(window, cx);
            });
        });
        self.editor_blur_subscriptions.push(subscription);
    }

    pub(crate) fn open_create_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let active_view = self
            .session()
            .map_or(ActiveView::AllItems, |session| session.active_view);
        let item_type = active_view.item_type().unwrap_or(ItemType::Login);
        self.open_create_editor_as(item_type, window, cx);
    }

    /// Same as [`Self::open_create_editor`] but with an explicit type,
    /// bypassing the active-view guess — used by the Home "Add item" menu.
    pub(crate) fn open_create_editor_as(
        &mut self,
        item_type: ItemType,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editor_blur_subscriptions.clear();
        let mut editor = crate::item_editor::ItemEditorState::for_create(window, cx);
        editor.item_type = item_type;
        for uris_focus in editor
            .uri_inputs
            .iter()
            .map(|input| input.focus_handle(cx))
            .collect::<Vec<_>>()
        {
            self.register_website_blur_listener(uris_focus, window, cx);
        }
        if let Some(session) = self.session_mut() {
            session.item_editor = Some(editor);
        }
        if self.uses_secure_note_workspace() || self.uses_login_workspace() {
            cx.notify();
        } else {
            self.open_item_editor_sheet(window, cx);
        }
    }

    pub(crate) fn open_editor_for_item(
        &mut self,
        item_id: ItemId,
        restore: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let AppState::Unlocked(session) = &self.state else {
            return;
        };
        let vault = &session.vault;
        self.editor_blur_subscriptions.clear();
        let result = if restore {
            crate::item_editor::ItemEditorState::for_restore(item_id, vault, window, cx)
        } else {
            crate::item_editor::ItemEditorState::for_edit(item_id, vault, window, cx)
        };
        match result {
            Ok(mut editor) => {
                // A previously fetched/uploaded image for this item, if any, was
                // persisted under its item key on save; picking it back up here
                // is what lets the reopened editor (and its trigger) show it
                // again instead of falling back to the type default.
                editor.local_icon = self
                    .local_icon_selections
                    .get(&editor.local_icon_key())
                    .cloned();
                for uris_focus in editor
                    .uri_inputs
                    .iter()
                    .map(|input| input.focus_handle(cx))
                    .collect::<Vec<_>>()
                {
                    self.register_website_blur_listener(uris_focus, window, cx);
                }
                if let Some(session) = self.session_mut() {
                    session.list.selected = Some(item_id);
                    session.item_editor = Some(editor);
                }
                if self.uses_login_workspace() || self.uses_secure_note_workspace() {
                    cx.notify();
                } else {
                    self.open_item_editor_sheet(window, cx);
                }
            }
            Err(error) => {
                if let Some(session) = self.session_mut() {
                    session.list.load =
                        ListLoadState::Failed(format!("Could not open item: {error}").into());
                }
            }
        }
        cx.notify();
    }

    fn move_selection(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.session_mut() else {
            return;
        };
        let list = &mut session.list;
        if list.filtered.is_empty() {
            return;
        }
        let current = list.selected_index().unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, list.filtered.len() as isize - 1) as usize;
        let item_id = list.items[list.filtered[next]].0;
        list.scroll_handle
            .scroll_to_item(next, gpui::ScrollStrategy::Center);
        self.select_item(item_id, cx);
        let _ = window;
    }

    /// Select an item to show its read-only detail panel, resetting any
    /// transient per-item UI state (password reveal, copy feedback).
    pub(crate) fn select_item(&mut self, item_id: ItemId, cx: &mut Context<Self>) {
        if let Some(session) = self.session_mut() {
            session.list.selected = Some(item_id);
            session.reveal_password = false;
        }
        cx.notify();
    }

    /// The search/filter/sort bar above the split view — full width, sitting
    /// above both the list and detail panes (Pencil "All Items Split Toolbar").
    pub(crate) fn render_vault_list_toolbar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let Some(list) = self.session().map(|session| &session.list) else {
            return div().into_any_element();
        };
        let search_input = list.search_input.clone();
        let sort_label = list.sort_mode.label();
        let locker = cx.entity();

        let search = Input::new(&search_input)
            .prefix(icon(IconName::Search, Some(15.), Some(theme.text_subtle)))
            .h(px(38.))
            .w(px(360.))
            .bg(theme.surface)
            .border_color(theme.field_border)
            .rounded(px(8.));

        let toolbar_button_content = |icon_path: &'static str, label: SharedString| {
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(
                    gpui_component::Icon::empty()
                        .path(icon_path)
                        .size(px(14.))
                        .text_color(theme.text_secondary),
                )
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(FontWeight(500.))
                        .text_color(theme.text_secondary)
                        .child(label),
                )
        };

        // ponytail: visual-only — there's no secondary filter facet (favorites,
        // custom tags, ...) to narrow by yet beyond the type pills below.
        let filter_button = Button::new("vault-list-filter")
            .ghost()
            .disabled(true)
            .h(px(36.))
            .px(px(11.))
            .rounded(px(7.))
            .bg(theme.surface)
            .border_1()
            .border_color(theme.field_border)
            .child(toolbar_button_content(
                "icons/list-filter.svg",
                "Filter".into(),
            ));

        let sort_button = Button::new("vault-list-sort")
            .ghost()
            .h(px(36.))
            .px(px(11.))
            .rounded(px(7.))
            .bg(theme.surface)
            .border_1()
            .border_color(theme.field_border)
            .on_click(move |_, _window, app| {
                locker.update(app, |locker, cx| {
                    if let Some(session) = locker.session_mut() {
                        session.list.toggle_sort_mode();
                    }
                    cx.notify();
                });
            })
            .child(toolbar_button_content(
                "icons/arrow-up-down.svg",
                sort_label.into(),
            ));

        div()
            .flex()
            .items_center()
            .justify_between()
            .w_full()
            .h(px(40.))
            .flex_shrink_0()
            .child(search)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(filter_button)
                    .child(sort_button),
            )
            .into_any_element()
    }

    /// The list pane itself: type filters, column header, and the scrollable
    /// item rows. A self-contained card sitting next to the detail pane.
    pub(crate) fn render_vault_list(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::current(cx);
        let card = |content: AnyElement| {
            div()
                .id("vault-list-panel")
                .flex()
                .flex_col()
                .w(px(716.))
                .flex_shrink_0()
                .h_full()
                .rounded(px(9.))
                .bg(theme.surface)
                .border_1()
                .border_color(theme.border)
                .overflow_hidden()
                .child(content)
        };

        let Some(session) = self.session() else {
            return card(
                div()
                    .p(px(16.))
                    .text_sm()
                    .text_color(theme.text_muted)
                    .child("No vault loaded")
                    .into_any_element(),
            )
            .into_any_element();
        };
        let list = &session.list;

        let load = list.load.clone();
        let item_count = list.filtered.len();
        let deleted_ids = list.deleted_ids.clone();
        let has_deleted = !deleted_ids.is_empty();
        let deleted_count = deleted_ids.len();
        let deleted_expanded = list.deleted_expanded;
        let active_type = list.type_filter;
        let health_filter = list.login_health_filter;
        let all_count = list.items.len();
        let logins_count = list.type_count(ItemType::Login);
        let notes_count = list.type_count(ItemType::SecureNote);
        let weak_count = list.weak_login_count();
        let reused_count = list.reused_login_count();
        let dupes_for_rows = duplicate_passwords(&list.items);
        let is_logins_view = session.active_view == ActiveView::Logins;
        let is_secure_notes_view = session.active_view == ActiveView::SecureNotes;
        let (scope_total, scope_noun) = match active_type {
            None => (all_count, "items"),
            Some(ItemType::Login) => (logins_count, "logins"),
            Some(ItemType::SecureNote) => (notes_count, "secure notes"),
        };
        let locker = cx.entity();

        // The Logins view swaps the generic type-filter pills for its own
        // "smart filters": real per-login password health, not a fabricated
        // "Personal"/"Work" tag the data model has no concept of — see
        // `LoginHealth`.
        let filters_row = if is_logins_view {
            div()
                .flex()
                .items_center()
                .gap(px(5.))
                .w_full()
                .h(px(44.))
                .px(px(12.))
                .flex_shrink_0()
                .child(
                    TypeFilterPill::new("login-filter-all", "All logins", logins_count)
                        .active(health_filter.is_none())
                        .on_click({
                            let locker = locker.clone();
                            move |_, _window, app| {
                                locker.update(app, |locker, cx| {
                                    if let Some(session) = locker.session_mut() {
                                        session.list.set_login_health_filter(None);
                                    }
                                    cx.notify();
                                });
                            }
                        })
                        .render(cx),
                )
                .child(
                    TypeFilterPill::new("login-filter-weak", "Weak", weak_count)
                        .count_color(theme.danger)
                        .active(health_filter == Some(LoginHealth::Weak))
                        .on_click({
                            let locker = locker.clone();
                            move |_, _window, app| {
                                locker.update(app, |locker, cx| {
                                    if let Some(session) = locker.session_mut() {
                                        session
                                            .list
                                            .set_login_health_filter(Some(LoginHealth::Weak));
                                    }
                                    cx.notify();
                                });
                            }
                        })
                        .render(cx),
                )
                .child(
                    TypeFilterPill::new("login-filter-reused", "Reused", reused_count)
                        .count_color(theme.danger)
                        .active(health_filter == Some(LoginHealth::Reused))
                        .on_click({
                            let locker = locker.clone();
                            move |_, _window, app| {
                                locker.update(app, |locker, cx| {
                                    if let Some(session) = locker.session_mut() {
                                        session
                                            .list
                                            .set_login_health_filter(Some(LoginHealth::Reused));
                                    }
                                    cx.notify();
                                });
                            }
                        })
                        .render(cx),
                )
                .into_any_element()
        } else if is_secure_notes_view {
            div()
                .flex()
                .items_center()
                .gap(px(5.))
                .w_full()
                .h(px(44.))
                .px(px(12.))
                .flex_shrink_0()
                .child(
                    TypeFilterPill::new("secure-note-filter-all", "All notes", notes_count)
                        .active(true)
                        .on_click(|_, _, _| {})
                        .render(cx),
                )
                .into_any_element()
        } else {
            div()
                .flex()
                .items_center()
                .gap(px(5.))
                .w_full()
                .h(px(44.))
                .px(px(12.))
                .flex_shrink_0()
                .child(
                    TypeFilterPill::new("type-filter-all", "All", all_count)
                        .active(active_type.is_none())
                        .on_click({
                            let locker = locker.clone();
                            move |_, _window, app| {
                                locker.update(app, |locker, cx| {
                                    if let Some(session) = locker.session_mut() {
                                        session.list.set_type_filter(None);
                                    }
                                    cx.notify();
                                });
                            }
                        })
                        .render(cx),
                )
                .child(
                    TypeFilterPill::new("type-filter-logins", "Logins", logins_count)
                        .active(active_type == Some(ItemType::Login))
                        .on_click({
                            let locker = locker.clone();
                            move |_, _window, app| {
                                locker.update(app, |locker, cx| {
                                    if let Some(session) = locker.session_mut() {
                                        session.list.set_type_filter(Some(ItemType::Login));
                                    }
                                    cx.notify();
                                });
                            }
                        })
                        .render(cx),
                )
                // ponytail: Cards/IDs have no backing item type yet — shown at
                // a real 0 rather than hidden, same convention as the sidebar.
                .child(TypeFilterPill::unavailable("type-filter-cards", "Cards").render(cx))
                .child(
                    TypeFilterPill::new("type-filter-notes", "Notes", notes_count)
                        .active(active_type == Some(ItemType::SecureNote))
                        .on_click({
                            let locker = locker.clone();
                            move |_, _window, app| {
                                locker.update(app, |locker, cx| {
                                    if let Some(session) = locker.session_mut() {
                                        session.list.set_type_filter(Some(ItemType::SecureNote));
                                    }
                                    cx.notify();
                                });
                            }
                        })
                        .render(cx),
                )
                .child(TypeFilterPill::unavailable("type-filter-ids", "IDs").render(cx))
                .into_any_element()
        };

        let row_locker = cx.entity();
        let data_dir = self.data_dir.clone();
        let rows = uniform_list("vault-list-rows", item_count, move |range, _window, app| {
            let data = range
                .filter_map(|index| {
                    let list = &row_locker.read(app).session()?.list;
                    let item_index = *list.filtered.get(index)?;
                    let (item_id, payload) = list.items.get(item_index)?;
                    Some((*item_id, payload.clone(), list.selected == Some(*item_id)))
                })
                .collect::<Vec<_>>();
            data.into_iter()
                .map(|(item_id, payload, selected)| {
                    let row_id: ElementId =
                        SharedString::from(format!("vault-list-row-{item_id}")).into();
                    let title = if payload.title.is_empty() {
                        "Untitled".to_owned()
                    } else {
                        payload.title.clone()
                    };
                    let local_selection = row_locker
                        .read(app)
                        .local_icon_selections
                        .get(&crate::icons::item_key(item_id));
                    let icon = crate::icons::render_resolved_icon(
                        crate::icons::resolved_item_icon(
                            &data_dir,
                            &crate::icons::item_key(item_id),
                            &payload,
                            local_selection,
                        ),
                        15.,
                        theme.text_secondary,
                    );
                    let updated = crate::app::relative_time(payload.updated_at);
                    let (subtitle, third_column) = if is_logins_view {
                        let (label, color) = login_health(theme, &payload, &dupes_for_rows);
                        (login_row_subtitle(&payload), (label.to_owned(), color))
                    } else {
                        let label = match payload.item_type {
                            ItemType::Login => "Login",
                            ItemType::SecureNote => "Secure note",
                        };
                        (
                            row_subtitle(&payload),
                            (label.to_owned(), theme.text_secondary),
                        )
                    };
                    let content = item_row_content(
                        theme,
                        icon,
                        title,
                        subtitle,
                        third_column,
                        updated,
                        selected,
                    );
                    let (bg, hover_bg) = if selected {
                        (theme.raised, theme.raised)
                    } else {
                        (theme.surface, theme.field)
                    };
                    let variant = ButtonCustomVariant::new(app).color(bg).hover(hover_bg);
                    Button::new(row_id)
                        .custom(variant)
                        .w_full()
                        .h(px(64.))
                        .px(px(14.))
                        .on_click({
                            let locker = row_locker.clone();
                            move |_, _window, app| {
                                locker.update(app, |locker, cx| locker.select_item(item_id, cx));
                            }
                        })
                        .child(content)
                })
                .collect()
        })
        .size_full();

        let deleted = deleted_ids.into_iter().enumerate().map(|(index, item_id)| {
            let row_id: ElementId = SharedString::from(format!("deleted-item-{item_id}")).into();
            Button::new(row_id)
                .ghost()
                .small()
                .w_full()
                .justify_start()
                .text_color(theme.text_muted)
                .label(format!("Preview & Restore — Deleted item {}", index + 1))
                .on_click({
                    let locker = locker.clone();
                    move |_, window, app| {
                        locker.update(app, |locker, cx| {
                            locker.open_editor_for_item(item_id, true, window, cx)
                        });
                    }
                })
        });
        let error = match load {
            ListLoadState::Ready => div(),
            ListLoadState::Failed(message) => div()
                .p(px(12.))
                .text_sm()
                .text_color(theme.danger)
                .child(message),
        };

        let (first_column, third_column) = if is_logins_view {
            ("ACCOUNT", "HEALTH")
        } else if is_secure_notes_view {
            ("NOTE", "CATEGORY")
        } else {
            ("ITEM", "TYPE")
        };

        // Pencil "Logins List Footer" — real counts (shown vs. total in scope,
        // plus the same weak/reused health this view's smart filters use).
        let footer = div()
            .flex()
            .items_center()
            .justify_between()
            .w_full()
            .h(px(40.))
            .px(px(14.))
            .flex_shrink_0()
            .bg(theme.inset)
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(theme.text_subtle)
                    .child(format!("{item_count} of {scope_total} {scope_noun} shown")),
            )
            .child(if is_logins_view {
                div()
                    .text_size(px(10.))
                    .text_color(theme.danger)
                    .child(format!(
                        "{weak_count} weak · {reused_count} reused passwords"
                    ))
                    .into_any_element()
            } else {
                div()
                    .text_size(px(10.))
                    .text_color(theme.text_secondary)
                    .child(if is_secure_notes_view {
                        "All notes encrypted"
                    } else {
                        ""
                    })
                    .into_any_element()
            });

        let body = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "up" => this.move_selection(-1, window, cx),
                    "down" => this.move_selection(1, window, cx),
                    "enter" => this.move_selection(0, window, cx),
                    _ => {}
                }
            }))
            .child(filters_row)
            .child(list_header_row(theme, first_column, third_column))
            .child(error)
            .child(if item_count == 0 {
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(theme.text_muted)
                    .child("No items match this view")
                    .into_any_element()
            } else {
                div().flex_1().min_h(px(0.)).child(rows).into_any_element()
            })
            .child(if !has_deleted {
                div().into_any_element()
            } else {
                div()
                    .id("deleted-items")
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .p(px(8.))
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        Button::new("deleted-section-toggle")
                            .ghost()
                            .small()
                            .w_full()
                            .justify_start()
                            .text_color(theme.text_muted)
                            .label(format!("Deleted ({deleted_count})"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(session) = this.session_mut() {
                                    session.list.toggle_deleted();
                                    cx.notify();
                                }
                            })),
                    )
                    .when(deleted_expanded, |section| section.children(deleted))
                    .into_any_element()
            })
            .child(footer)
            .into_any_element();

        card(body).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login(password: &str) -> ItemPayload {
        ItemPayload {
            schema_version: nox_core::ITEM_SCHEMA_VERSION,
            item_type: ItemType::Login,
            title: "Example".into(),
            username: "alex".into(),
            password: password.into(),
            uris: vec![],
            notes: String::new(),
            created_at: 1,
            updated_at: 1,
            icon: nox_core::IconChoice::Default,
        }
    }

    #[test]
    fn short_passwords_are_weak_empty_ones_are_not() {
        assert!(is_weak_password("short1"));
        assert!(!is_weak_password("longenoughpassword"));
        // Empty means "not set", not "weak" — there's nothing to judge yet.
        assert!(!is_weak_password(""));
    }

    #[test]
    fn duplicate_passwords_ignores_empty_and_non_login_items() {
        let items = vec![
            (ItemId::new(), login("shared-secret")),
            (ItemId::new(), login("shared-secret")),
            (ItemId::new(), login("unique-one")),
            (ItemId::new(), login("")),
            (ItemId::new(), login("")),
            (ItemId::new(), {
                let mut note = login("shared-secret");
                note.item_type = ItemType::SecureNote;
                note
            }),
        ];
        let dupes = duplicate_passwords(&items);
        assert!(dupes.contains("shared-secret"));
        assert!(!dupes.contains("unique-one"));
        assert!(!dupes.contains(""));
    }

    #[test]
    fn secure_notes_only_show_backed_filters_and_encryption_status() {
        let source = include_str!("vault_list.rs");
        let start = source
            .find("} else if is_secure_notes_view {")
            .expect("secure-note filter branch");
        let secure_note_branch = &source[start
            ..source[start..]
                .find("} else {")
                .expect("end of secure-note filter branch")
                + start];

        assert!(!secure_note_branch.contains("Personal"));
        assert!(!secure_note_branch.contains("Work"));
        assert!(!secure_note_branch.contains("Recovery"));
        assert!(source.contains("All notes encrypted"));
    }

    #[test]
    fn login_health_prioritizes_reused_over_weak() {
        let theme = Theme::cipher_midnight();
        let dupes = HashSet::from(["short".to_owned()]);
        // Short *and* reused — reused should win, since it's determined first.
        assert_eq!(login_health(theme, &login("short"), &dupes).0, "Reused");
        assert_eq!(
            login_health(theme, &login("longenoughpassword"), &dupes).0,
            "Strong"
        );
        assert_eq!(
            login_health(theme, &login("nodupe12"), &HashSet::new()).0,
            "Strong"
        );
        assert_eq!(
            login_health(theme, &login("short2"), &HashSet::new()).0,
            "Weak"
        );
        assert_eq!(login_health(theme, &login(""), &HashSet::new()).0, "—");
    }
}
