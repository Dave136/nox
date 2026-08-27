use super::Locker;
use gpui::{ClipboardItem, Context, Task, Window};
use locker_core::{ItemId, SecretBytes};
use std::time::Duration;

pub const DEFAULT_CLIPBOARD_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a row's "Copied" confirmation stays visible before reverting.
const COPY_FEEDBACK_DURATION: Duration = Duration::from_millis(1200);

/// Which field of an item was last copied, for row-level "Copied" feedback.
/// `Uri` carries the index since an item can have more than one website.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CopyField {
    Username,
    Password,
    Uri(usize),
}

pub(crate) struct ClipboardState {
    pub(crate) timeout: Duration,
    pub(crate) epoch: u64,
    pub(crate) expected: Option<SecretBytes>,
    pub(crate) clear_task: Task<()>,
    /// The item/field whose row should currently show "Copied", if any.
    pub(crate) feedback: Option<(ItemId, CopyField)>,
    feedback_epoch: u64,
    _feedback_task: Task<()>,
}

impl ClipboardState {
    pub(crate) fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            epoch: 0,
            expected: None,
            clear_task: Task::ready(()),
            feedback: None,
            feedback_epoch: 0,
            _feedback_task: Task::ready(()),
        }
    }
}

impl Locker {
    pub(crate) fn copy_username(
        &mut self,
        item_id: ItemId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(&self.state, super::AppState::Unlocked(_)) {
            return;
        }
        let Some(value) = self
            .vault_list
            .as_ref()
            .and_then(|list| list.items.iter().find(|(id, _)| *id == item_id))
            .map(|(_, payload)| payload.username.clone())
            .filter(|value| !value.is_empty())
        else {
            return;
        };
        self.copy_secret(SecretBytes::new(value.as_bytes()), window, cx);
        self.show_copy_feedback(item_id, CopyField::Username, window, cx);
    }

    pub(crate) fn copy_password(
        &mut self,
        item_id: ItemId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !matches!(&self.state, super::AppState::Unlocked(_)) {
            return;
        }
        let Some(value) = self
            .vault_list
            .as_ref()
            .and_then(|list| list.items.iter().find(|(id, _)| *id == item_id))
            .map(|(_, payload)| payload.password.clone())
            .filter(|value| !value.is_empty())
        else {
            return;
        };
        self.copy_secret(SecretBytes::new(value.as_bytes()), window, cx);
        self.show_copy_feedback(item_id, CopyField::Password, window, cx);
    }

    /// Websites aren't secrets, so this skips `copy_secret`'s auto-clear
    /// timeout — just a plain clipboard write plus the same row feedback.
    pub(crate) fn copy_uri(
        &mut self,
        item_id: ItemId,
        index: usize,
        uri: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if uri.is_empty() {
            return;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(uri));
        self.show_copy_feedback(item_id, CopyField::Uri(index), window, cx);
    }

    fn show_copy_feedback(
        &mut self,
        item_id: ItemId,
        field: CopyField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.clipboard.feedback_epoch = self.clipboard.feedback_epoch.wrapping_add(1);
        let epoch = self.clipboard.feedback_epoch;
        self.clipboard.feedback = Some((item_id, field));
        self.clipboard._feedback_task = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(COPY_FEEDBACK_DURATION).await;
            if let Some(this) = this.upgrade() {
                let _ = cx.update(|_, app| {
                    this.update(app, |this, cx| {
                        if this.clipboard.feedback_epoch == epoch {
                            this.clipboard.feedback = None;
                            cx.notify();
                        }
                    });
                });
            }
        });
        cx.notify();
    }

    pub(crate) fn copy_secret(
        &mut self,
        value: SecretBytes,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if value.is_empty() {
            return;
        }
        self.clipboard.epoch = self.clipboard.epoch.wrapping_add(1);
        let epoch = self.clipboard.epoch;
        self.clipboard.expected = Some(value.clone());
        let text = String::from_utf8_lossy(value.as_bytes()).into_owned();
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        let timeout = self.clipboard.timeout;
        self.clipboard.clear_task = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(timeout).await;
            if let Some(this) = this.upgrade() {
                let _ = cx.update(|_, app| {
                    this.update(app, |this, cx| {
                        if this.clipboard.epoch == epoch {
                            this.clear_clipboard_if_unchanged(cx);
                        }
                    });
                });
            }
        });
        self.note_activity(cx);
        cx.notify();
    }

    pub(crate) fn clear_clipboard_if_unchanged(&mut self, cx: &mut Context<Self>) {
        if let Some(expected) = self.clipboard.expected.as_ref() {
            let current = cx.read_from_clipboard().and_then(|item| item.text());
            if current.as_deref().map(str::as_bytes) == Some(expected.as_bytes()) {
                cx.write_to_clipboard(ClipboardItem::new_string(String::new()));
            }
        }
        self.clipboard.expected = None;
        self.clipboard.epoch = self.clipboard.epoch.wrapping_add(1);
        self.clipboard.clear_task = Task::ready(());
        cx.notify();
    }

    pub(crate) fn discard_clipboard_state(&mut self, cx: &mut Context<Self>) {
        self.clear_clipboard_if_unchanged(cx);
    }
}
