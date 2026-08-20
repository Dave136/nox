use super::{AppState, Locker};
use gpui::{AnyElement, Context, SharedString, Window, div, prelude::*};
use gpui_component::{
    Disableable,
    button::Button,
    radio::{Radio, RadioGroup},
};
use locker_core::{ChangeId, ItemId, ItemPayload, ItemType, VaultError};

#[derive(Clone)]
pub(crate) struct ConflictChoice {
    pub(crate) change_id: ChangeId,
    pub(crate) payload: Option<ItemPayload>,
    pub(crate) summary: ConflictChoiceSummary,
}

#[derive(Clone)]
pub(crate) struct ConflictChoiceSummary {
    pub(crate) label: SharedString,
    pub(crate) detail: SharedString,
}

pub(crate) struct ItemConflict {
    pub(crate) item_id: ItemId,
    pub(crate) choices: Vec<ConflictChoice>,
    pub(crate) selected: Option<ChangeId>,
    pub(crate) error: Option<SharedString>,
}

pub(crate) enum ConflictState {
    Closed,
    Ready(Vec<ItemConflict>),
    Failed(SharedString),
}

impl ConflictState {
    pub(crate) fn count(&self) -> usize {
        match self {
            Self::Ready(conflicts) => conflicts.len(),
            Self::Closed | Self::Failed(_) => 0,
        }
    }

    #[allow(clippy::type_complexity)]
    pub(crate) fn from_initial_load(
        input: Result<Vec<(ItemId, Vec<(ChangeId, Option<ItemPayload>)>)>, VaultError>,
    ) -> Self {
        let Ok(conflicts) = input else {
            return Self::Failed("Conflicts could not be loaded. Lock and unlock to retry.".into());
        };
        let mut result = Vec::with_capacity(conflicts.len());
        for (item_id, choices) in conflicts {
            let mut seen = std::collections::HashSet::new();
            let mut mapped = Vec::with_capacity(choices.len());
            for (change_id, payload) in choices {
                if !seen.insert(change_id) {
                    return Self::Failed(
                        "Conflicts could not be loaded. Lock and unlock to retry.".into(),
                    );
                }
                mapped.push(ConflictChoice {
                    change_id,
                    summary: summarize(payload.as_ref()),
                    payload,
                });
            }
            result.push(ItemConflict {
                item_id,
                choices: mapped,
                selected: None,
                error: None,
            });
        }
        Self::Ready(result)
    }
}

fn summarize(payload: Option<&ItemPayload>) -> ConflictChoiceSummary {
    let Some(payload) = payload else {
        return ConflictChoiceSummary {
            label: "Deleted revision".into(),
            detail: "Delete this item".into(),
        };
    };
    let kind = match payload.item_type {
        ItemType::Login => "Login",
        ItemType::SecureNote => "Secure note",
    };
    let title = truncate(&payload.title, 80);
    let mut detail = format!("{kind} · {title}");
    if !payload.username.is_empty() {
        detail.push_str(" · ");
        detail.push_str(&truncate(&payload.username, 80));
    }
    if !payload.uris.is_empty() {
        detail.push_str(" · ");
        detail.push_str(&truncate(&payload.uris.join(", "), 100));
    }
    if !payload.notes.is_empty() {
        detail.push_str(" · ");
        detail.push_str(&truncate(&payload.notes, 100));
    }
    ConflictChoiceSummary {
        label: title.into(),
        detail: detail.into(),
    }
}

fn truncate(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

impl Locker {
    pub(crate) fn open_conflicts(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.conflicts_open = true;
        self.note_activity(cx);
        cx.notify();
    }

    pub(crate) fn select_conflict_choice(
        &mut self,
        item_id: ItemId,
        change_id: ChangeId,
        cx: &mut Context<Self>,
    ) {
        if let ConflictState::Ready(conflicts) = &mut self.conflicts
            && let Some(conflict) = conflicts
                .iter_mut()
                .find(|conflict| conflict.item_id == item_id)
            && conflict
                .choices
                .iter()
                .any(|choice| choice.change_id == change_id)
        {
            conflict.selected = Some(change_id);
            conflict.error = None;
            self.note_activity(cx);
            cx.notify();
        }
    }

    pub(crate) fn resolve_selected_conflict(
        &mut self,
        item_id: ItemId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((selected_change_id, selected_payload)) = (match &self.conflicts {
            ConflictState::Ready(conflicts) => conflicts
                .iter()
                .find(|conflict| conflict.item_id == item_id)
                .and_then(|conflict| conflict.selected)
                .and_then(|id| {
                    conflicts
                        .iter()
                        .find(|conflict| conflict.item_id == item_id)
                        .and_then(|conflict| {
                            conflict
                                .choices
                                .iter()
                                .find(|choice| choice.change_id == id)
                        })
                        .map(|choice| (id, choice.payload.clone()))
                }),
            _ => None,
        }) else {
            return;
        };
        let result = match &mut self.state {
            AppState::Unlocked(vault) => vault.resolve_conflict(item_id, selected_change_id),
            _ => return,
        };
        if result.is_err() {
            if let ConflictState::Ready(conflicts) = &mut self.conflicts
                && let Some(conflict) = conflicts
                    .iter_mut()
                    .find(|conflict| conflict.item_id == item_id)
            {
                conflict.error = Some("Could not resolve this conflict.".into());
            }
            cx.notify();
            return;
        }
        if let ConflictState::Ready(conflicts) = &mut self.conflicts {
            conflicts.retain(|conflict| conflict.item_id != item_id);
            if conflicts.is_empty() {
                self.conflicts_open = false;
                window.on_next_frame(|window, _| window.focus_next());
            }
        }
        if let Some(list) = self.vault_list.as_mut() {
            if let Some(payload) = selected_payload {
                list.upsert(item_id, payload);
                list.remove_deleted(item_id);
                list.selected = Some(item_id);
            } else {
                list.remove(item_id);
                list.add_deleted(item_id);
                list.selected = None;
                if let Some(editor) = self.item_editor.as_ref()
                    && matches!(editor.mode, super::item_editor::EditorMode::Edit(id) | super::item_editor::EditorMode::Restore(id) if id == item_id)
                {
                    self.item_editor = None;
                }
            }
        }
        self.note_activity(cx);
        cx.notify();
    }

    pub(crate) fn render_conflicts(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let ConflictState::Failed(message) = &self.conflicts {
            return div().child(message.clone()).into_any_element();
        }
        let ConflictState::Ready(conflicts) = &self.conflicts else {
            return div().child("No conflicts.").into_any_element();
        };
        if conflicts.is_empty() {
            return div().child("No conflicts.").into_any_element();
        }
        let locker = cx.entity();
        let groups = conflicts.iter().map(|conflict| {
            let item_id = conflict.item_id;
            let selected = conflict.selected;
            let error = conflict.error.clone();
            let choices: Vec<_> = conflict
                .choices
                .iter()
                .map(|choice| (choice.change_id, choice.summary.clone()))
                .collect();
            let radios = choices.iter().enumerate().map(|(index, choice)| {
                Radio::new(SharedString::from(format!("conflict-{item_id}-{index}")))
                    .label(format!("{} — {}", choice.1.label, choice.1.detail))
            });
            let selected_index = choices.iter().position(|choice| Some(choice.0) == selected);
            let group_locker = locker.clone();
            let radio_group =
                RadioGroup::vertical(SharedString::from(format!("conflict-group-{item_id}")))
                    .children(radios)
                    .selected_index(selected_index)
                    .on_click(move |index, _, app| {
                        if let Some((change_id, _)) = choices.get(*index) {
                            group_locker.update(app, |locker, cx| {
                                locker.select_conflict_choice(item_id, *change_id, cx)
                            });
                        }
                    });
            let resolve_locker = locker.clone();
            div()
                .id(SharedString::from(format!("conflict-item-{item_id}")))
                .flex()
                .flex_col()
                .gap_1()
                .child(format!("Conflict for {item_id}"))
                .child(radio_group)
                .child(
                    error
                        .map(|message| div().child(message))
                        .unwrap_or_else(div),
                )
                .child(
                    Button::new(SharedString::from(format!("resolve-{item_id}")))
                        .label("Resolve")
                        .disabled(selected.is_none())
                        .on_click(move |_, window, app| {
                            resolve_locker.update(app, |locker, cx| {
                                locker.resolve_selected_conflict(item_id, window, cx)
                            });
                        }),
                )
        });
        div()
            .id("conflicts-panel")
            .flex()
            .flex_col()
            .gap_2()
            .children(groups)
            .into_any_element()
    }
}
