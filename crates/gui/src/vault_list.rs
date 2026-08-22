use super::{AppState, Locker};
use gpui::{
    AnyElement, Context, ElementId, Entity, FontWeight, KeyDownEvent, SharedString, Subscription,
    UniformListScrollHandle, Window, div, prelude::*, px, uniform_list,
};
use gpui_component::{
    ActiveTheme, Sizable,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
};
use locker_core::{ItemId, ItemPayload, VaultError};

use crate::assets::{IconName, icon};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ListLoadState {
    Ready,
    Failed(SharedString),
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
    pub(crate) _search_subscription: Subscription,
}

impl VaultListState {
    pub(crate) fn from_initial_load(
        items: Result<Vec<(ItemId, ItemPayload)>, VaultError>,
        deleted_ids: Result<Vec<ItemId>, VaultError>,
        window: &mut Window,
        cx: &mut Context<Locker>,
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
        let filtered = (0..items.len()).collect();
        let _search_subscription =
            cx.subscribe(&search_input, |locker, _input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    locker.recompute_vault_list_filter(cx);
                }
            });
        Self {
            load,
            items,
            filtered,
            deleted_ids,
            search_input,
            search_query_lower: String::new(),
            selected: None,
            deleted_expanded: false,
            scroll_handle: UniformListScrollHandle::new(),
            _search_subscription,
        }
    }

    pub(crate) fn recompute_filter(&mut self, query: String) {
        if self.search_query_lower == query {
            return;
        }
        self.search_query_lower = query;
        self.rebuild_filter();
    }

    fn rebuild_filter(&mut self) {
        self.filtered = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, (_, payload))| {
                payload
                    .title
                    .to_lowercase()
                    .contains(&self.search_query_lower)
                    || payload
                        .username
                        .to_lowercase()
                        .contains(&self.search_query_lower)
            })
            .map(|(index, _)| index)
            .collect();
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
}

impl Locker {
    pub(crate) fn recompute_vault_list_filter(&mut self, cx: &mut Context<Self>) {
        let Some(list) = self.vault_list.as_mut() else {
            return;
        };
        let query = list.search_input.read(cx).value().to_lowercase();
        if list.search_query_lower == query {
            return;
        }
        list.recompute_filter(query);
        cx.notify();
    }

    pub(crate) fn open_create_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.item_editor = Some(super::item_editor::ItemEditorState::for_create(window, cx));
        cx.notify();
    }

    pub(crate) fn open_editor_for_item(
        &mut self,
        item_id: ItemId,
        restore: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let AppState::Unlocked(vault) = &self.state else {
            return;
        };
        let result = if restore {
            super::item_editor::ItemEditorState::for_restore(item_id, vault, window, cx)
        } else {
            super::item_editor::ItemEditorState::for_edit(item_id, vault, window, cx)
        };
        match result {
            Ok(editor) => {
                if let Some(list) = self.vault_list.as_mut() {
                    list.selected = Some(item_id);
                }
                self.item_editor = Some(editor);
            }
            Err(error) => {
                if let Some(list) = self.vault_list.as_mut() {
                    list.load =
                        ListLoadState::Failed(format!("Could not open item: {error}").into());
                }
            }
        }
        cx.notify();
    }

    fn move_selection(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(list) = self.vault_list.as_mut() else {
            return;
        };
        if list.filtered.is_empty() {
            return;
        }
        let current = list.selected_index().unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, list.filtered.len() as isize - 1) as usize;
        let item_id = list.items[list.filtered[next]].0;
        list.selected = Some(item_id);
        list.scroll_handle
            .scroll_to_item(next, gpui::ScrollStrategy::Center);
        if delta == 0 {
            self.open_editor_for_item(item_id, false, window, cx);
        }
        cx.notify();
    }

    pub(crate) fn render_vault_list(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let sidebar_bg = theme.muted;
        let border = theme.border;
        let foreground = theme.foreground;
        let muted_foreground = theme.muted_foreground;
        let danger = theme.danger;
        let selected_bg = theme.list_active;
        let selected_border = theme.list_active_border;

        let Some(list) = self.vault_list.as_ref() else {
            return div()
                .id("vault-list-panel")
                .w(px(280.))
                .h_full()
                .flex_shrink_0()
                .bg(sidebar_bg)
                .border_r_1()
                .border_color(border)
                .p(px(16.))
                .text_sm()
                .text_color(muted_foreground)
                .child("No vault loaded")
                .into_any_element();
        };
        let search_input = list.search_input.clone();
        let load = list.load.clone();
        let item_count = list.filtered.len();
        let deleted_ids = list.deleted_ids.clone();
        let has_deleted = !deleted_ids.is_empty();
        let deleted_count = deleted_ids.len();
        let deleted_expanded = list.deleted_expanded;
        let locker = cx.entity();
        let row_locker = locker.clone();
        let rows = uniform_list("vault-list-rows", item_count, move |range, _window, app| {
            let data = range
                .filter_map(|index| {
                    let list = row_locker.read(app).vault_list.as_ref()?;
                    let item_index = *list.filtered.get(index)?;
                    let (item_id, payload) = list.items.get(item_index)?;
                    Some((
                        *item_id,
                        payload.title.clone(),
                        payload.username.clone(),
                        list.selected == Some(*item_id),
                    ))
                })
                .collect::<Vec<_>>();
            data.into_iter()
                .map(|(item_id, title, username, selected)| {
                    let row_id: ElementId =
                        SharedString::from(format!("vault-list-row-{item_id}")).into();
                    let title_text = if title.is_empty() {
                        "Untitled".to_owned()
                    } else {
                        title
                    };
                    let mut button = Button::new(row_id)
                        .ghost()
                        .w_full()
                        .h(px(52.))
                        .justify_start()
                        .px(px(10.))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(2.))
                                .w_full()
                                .overflow_hidden()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(foreground)
                                        .truncate()
                                        .child(title_text),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(muted_foreground)
                                        .truncate()
                                        .child(if username.is_empty() {
                                            "—".to_owned()
                                        } else {
                                            username
                                        }),
                                ),
                        );
                    if selected {
                        button = button
                            .bg(selected_bg)
                            .border_1()
                            .border_color(selected_border);
                    }
                    button.on_click({
                        let locker = row_locker.clone();
                        move |_, window, app| {
                            locker.update(app, |locker, cx| {
                                locker.open_editor_for_item(item_id, false, window, cx)
                            });
                        }
                    })
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
                .text_color(muted_foreground)
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
            ListLoadState::Failed(message) => div().text_sm().text_color(danger).child(message),
        };
        div()
            .id("vault-list-panel")
            .flex()
            .flex_col()
            .gap(px(12.))
            .w(px(280.))
            .h_full()
            .flex_shrink_0()
            .bg(sidebar_bg)
            .border_r_1()
            .border_color(border)
            .p(px(12.))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "up" => this.move_selection(-1, window, cx),
                    "down" => this.move_selection(1, window, cx),
                    "enter" => this.move_selection(0, window, cx),
                    _ => {}
                }
            }))
            .child(
                Input::new(&search_input)
                    .prefix(icon(IconName::Search, Some(14.), Some(muted_foreground))),
            )
            .child(
                Button::new("new-item")
                    .primary()
                    .w_full()
                    .label("New Item")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_create_editor(window, cx)),
                    ),
            )
            .child(error)
            .child(div().flex_1().min_h(px(0.)).child(rows))
            .child(if !has_deleted {
                div().into_any_element()
            } else {
                div()
                    .id("deleted-items")
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .pt(px(8.))
                    .border_t_1()
                    .border_color(border)
                    .child(
                        Button::new("deleted-section-toggle")
                            .ghost()
                            .small()
                            .w_full()
                            .justify_start()
                            .text_color(muted_foreground)
                            .label(format!("Deleted ({deleted_count})"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(list) = this.vault_list.as_mut() {
                                    list.toggle_deleted();
                                    cx.notify();
                                }
                            })),
                    )
                    .when(deleted_expanded, |section| section.children(deleted))
                    .into_any_element()
            })
            .into_any_element()
    }
}
