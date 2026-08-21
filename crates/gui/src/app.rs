#[path = "backup.rs"]
mod backup;
#[path = "clipboard.rs"]
mod clipboard;
#[path = "conflicts.rs"]
mod conflicts;
#[path = "item_editor.rs"]
mod item_editor;
#[path = "ui/mod.rs"]
pub mod ui;
#[path = "vault_list.rs"]
mod vault_list;

use backup::BackupState;
use clipboard::ClipboardState;
pub use clipboard::DEFAULT_CLIPBOARD_TIMEOUT;
use conflicts::ConflictState;
use item_editor::ItemEditorState;
use ui::window::controls::{OpenCommandPalette, WindowCommand, WindowControls};
use vault_list::VaultListState;

use gpui::{
    Context, Entity, KeyBinding, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, Render,
    SharedString, Task, Window, div, prelude::*, px,
};
use gpui_component::{
    Disableable, Root, WindowExt,
    button::Button,
    input::{Input, InputState},
};
use gpui_rsx::rsx;
use locker_core::{SecretBytes, Vault, VaultError};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

/// Default duration before an inactive unlocked vault is locked.
pub const DEFAULT_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(300);

const UNLOCK_ERROR_MESSAGE: &str = "Incorrect password or corrupted vault.";
const CANNOT_RECOVER_PASSWORD_NOTICE: &str =
    "Locker cannot recover a forgotten master password. Store it somewhere safe.";

/// The top-level vault lifecycle state.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum AppState {
    NoVault,
    Locked,
    Unlocked(Vault),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum FormState {
    Idle,
    Pending,
    Error(SharedString),
}

/// Root Locker view for vault creation, unlock, and lock lifecycle actions.
pub struct Locker {
    state: AppState,
    vault_path: PathBuf,

    create_password: Entity<InputState>,
    create_confirm: Entity<InputState>,
    create_state: FormState,
    _create_task: Task<()>,

    unlock_password: Entity<InputState>,
    unlock_state: FormState,
    _unlock_task: Task<()>,

    inactivity_timeout: Duration,
    last_activity: Instant,
    inactivity_epoch: usize,
    _inactivity_task: Task<()>,

    pub(crate) vault_list: Option<VaultListState>,
    pub(crate) item_editor: Option<ItemEditorState>,
    pub(crate) clipboard: ClipboardState,
    pub(crate) conflicts: ConflictState,
    pub(crate) conflicts_open: bool,
    pub(crate) backup: BackupState,
    pub(crate) window_controls: Entity<WindowControls>,
}

impl Locker {
    /// Construct a Locker view for an already-resolved vault path.
    pub fn new(
        vault_path: PathBuf,
        inactivity_timeout: Duration,
        clipboard_timeout: Duration,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let state = if vault_path.exists() {
            AppState::Locked
        } else {
            AppState::NoVault
        };
        let create_password = Self::new_input(window, cx, "Password");
        let create_confirm = Self::new_input(window, cx, "Confirm password");
        let unlock_password = Self::new_input(window, cx, "Password");
        let locker = cx.weak_entity();
        let window_controls = cx.new(|cx| {
            WindowControls::new(window, cx).with_command_handler(move |command, window, app| {
                let _ = locker.update(app, |locker, cx| {
                    locker.run_window_command(command, window, cx);
                });
            })
        });
        cx.bind_keys([KeyBinding::new("ctrl-p", OpenCommandPalette, None)]);
        let locker = Self {
            state,
            vault_path,
            create_password,
            create_confirm,
            create_state: FormState::Idle,
            _create_task: Task::ready(()),
            unlock_password,
            unlock_state: FormState::Idle,
            _unlock_task: Task::ready(()),
            inactivity_timeout,
            last_activity: Instant::now(),
            inactivity_epoch: 0,
            _inactivity_task: Task::ready(()),
            vault_list: None,
            item_editor: None,
            clipboard: ClipboardState::new(clipboard_timeout),
            conflicts: ConflictState::Closed,
            conflicts_open: false,
            backup: BackupState::new(),
            window_controls,
        };

        match locker.state {
            AppState::NoVault => Self::focus_input(&locker.create_password, window, cx),
            AppState::Locked => Self::focus_input(&locker.unlock_password, window, cx),
            AppState::Unlocked(_) => {}
        }
        locker
    }

    fn new_input(
        window: &mut Window,
        cx: &mut Context<Self>,
        placeholder: &'static str,
    ) -> Entity<InputState> {
        cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(placeholder)
        })
    }

    fn focus_input(input: &Entity<InputState>, window: &mut Window, cx: &mut Context<Self>) {
        input.update(cx, |input, cx| input.focus(window, cx));
    }

    fn reset_create_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.create_password = Self::new_input(window, cx, "Password");
        self.create_confirm = Self::new_input(window, cx, "Confirm password");
    }

    fn reset_unlock_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.unlock_password = Self::new_input(window, cx, "Password");
    }

    fn create_vault(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.create_state == FormState::Pending || !self.backup.is_idle() {
            return;
        }

        let password = self.create_password.read(cx).value();
        let confirmation = self.create_confirm.read(cx).value();
        let secret = SecretBytes::new(password.as_bytes());
        // Replace the entities even on validation errors so failed submissions
        // cannot remain in InputState's undo history.
        self.reset_create_inputs(window, cx);

        if password.is_empty() {
            self.create_state = FormState::Error("Enter a password.".into());
            Self::focus_input(&self.create_password, window, cx);
            cx.notify();
            return;
        }
        if password != confirmation {
            self.create_state = FormState::Error("Passwords do not match.".into());
            Self::focus_input(&self.create_password, window, cx);
            cx.notify();
            return;
        }

        self.create_state = FormState::Pending;
        cx.notify();
        let path = self.vault_path.clone();
        self._create_task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let vault = Vault::create(secret.as_bytes(), &path)?;
                    let items = vault.list_items();
                    let deleted = vault.list_deleted_items();
                    let conflicts = vault.list_conflicts();
                    Ok::<_, VaultError>((vault, items, deleted, conflicts))
                })
                .await;
            if let Some(this) = this.upgrade() {
                cx.update(|window, app| {
                    this.update(app, |this, cx| match result {
                        Ok((vault, items, deleted, conflicts)) => {
                            this.state = AppState::Unlocked(vault);
                            this.vault_list = Some(VaultListState::from_initial_load(
                                items, deleted, window, cx,
                            ));
                            this.item_editor = None;
                            this.conflicts = ConflictState::from_initial_load(conflicts);
                            this.conflicts_open = false;
                            this.create_state = FormState::Idle;
                            this.arm_inactivity_timer(window, cx);
                            window.on_next_frame(|window, cx| window.focus_next(cx));
                            cx.notify();
                        }
                        Err(VaultError::VaultAlreadyExists) => {
                            this.create_state =
                                FormState::Error("A vault already exists at this location.".into());
                            Self::focus_input(&this.create_password, window, cx);
                            cx.notify();
                        }
                        Err(error) => {
                            eprintln!("vault creation failed: {error}");
                            this.create_state =
                                FormState::Error("Could not create the vault. Try again.".into());
                            Self::focus_input(&this.create_password, window, cx);
                            cx.notify();
                        }
                    });
                })
                .ok();
            }
        });
    }

    fn unlock_vault(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.unlock_state == FormState::Pending || !self.backup.is_idle() {
            return;
        }

        let password = self.unlock_password.read(cx).value();
        let secret = SecretBytes::new(password.as_bytes());
        self.reset_unlock_input(window, cx);
        self.unlock_state = FormState::Pending;
        cx.notify();
        let path = self.vault_path.clone();
        self._unlock_task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let vault = Vault::unlock(secret.as_bytes(), &path)?;
                    let items = vault.list_items();
                    let deleted = vault.list_deleted_items();
                    let conflicts = vault.list_conflicts();
                    Ok::<_, VaultError>((vault, items, deleted, conflicts))
                })
                .await;
            if let Some(this) = this.upgrade() {
                cx.update(|window, app| {
                    this.update(app, |this, cx| match result {
                        Ok((vault, items, deleted, conflicts)) => {
                            this.state = AppState::Unlocked(vault);
                            this.vault_list = Some(VaultListState::from_initial_load(
                                items, deleted, window, cx,
                            ));
                            this.item_editor = None;
                            this.conflicts = ConflictState::from_initial_load(conflicts);
                            this.conflicts_open = false;
                            this.unlock_state = FormState::Idle;
                            this.arm_inactivity_timer(window, cx);
                            window.on_next_frame(|window, cx| window.focus_next(cx));
                            cx.notify();
                        }
                        Err(error) => {
                            eprintln!("vault unlock failed: {error}");
                            this.unlock_state = FormState::Error(UNLOCK_ERROR_MESSAGE.into());
                            Self::focus_input(&this.unlock_password, window, cx);
                            cx.notify();
                        }
                    });
                })
                .ok();
            }
        });
    }

    fn note_activity(&mut self, cx: &mut Context<Self>) {
        if matches!(&self.state, AppState::Unlocked(_)) {
            self.last_activity = cx.background_executor().now();
        }
    }

    fn arm_inactivity_timer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The executor clock is monotonic in production and advanceable in tests.
        self.last_activity = cx.background_executor().now();
        self.inactivity_epoch += 1;
        let epoch = self.inactivity_epoch;
        let timeout = self.inactivity_timeout;
        self._inactivity_task = cx.spawn_in(window, async move |this, cx| {
            loop {
                let Some(remaining) = cx
                    .update(|window, app| {
                        this.update(app, |this, cx| {
                            if this.inactivity_epoch != epoch
                                || !matches!(&this.state, AppState::Unlocked(_))
                            {
                                return None;
                            }
                            let elapsed = cx
                                .background_executor()
                                .now()
                                .saturating_duration_since(this.last_activity);
                            if elapsed >= timeout {
                                this.lock_vault(window, cx);
                                None
                            } else {
                                Some(timeout - elapsed)
                            }
                        })
                    })
                    .ok()
                    .and_then(Result::ok)
                    .flatten()
                else {
                    return;
                };
                cx.background_executor().timer(remaining).await;
            }
        });
    }

    fn lock_vault(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(&self.state, AppState::Unlocked(_)) {
            return;
        }
        window.close_all_dialogs(cx);
        self.discard_clipboard_state(cx);
        self.inactivity_epoch += 1;
        self._inactivity_task = Task::ready(());
        self.vault_list = None;
        self.item_editor = None;
        self.conflicts = ConflictState::Closed;
        self.conflicts_open = false;
        if matches!(
            self.backup.operation,
            backup::BackupOperation::ChoosingExportPath
                | backup::BackupOperation::Exporting
                | backup::BackupOperation::ChoosingRestorePath
                | backup::BackupOperation::AwaitingRestoreConfirmation { .. }
        ) {
            self.backup.epoch = self.backup.epoch.wrapping_add(1);
            self.backup.operation = backup::BackupOperation::Idle;
            self.backup.dialog = None;
            self.backup.task = Task::ready(());
        }
        if let AppState::Unlocked(vault) = std::mem::replace(&mut self.state, AppState::Locked) {
            vault.lock();
        }
        Self::focus_input(&self.unlock_password, window, cx);
        cx.notify();
    }

    fn run_window_command(
        &mut self,
        command: WindowCommand,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        match command {
            WindowCommand::Minimize => window.minimize_window(),
            WindowCommand::ToggleMaximize => window.zoom_window(),
            WindowCommand::Close => window.remove_window(),
        }
    }

    fn render_no_vault(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pending = self.create_state == FormState::Pending;
        let backup_busy = !self.backup.is_idle();
        let error = match &self.create_state {
            FormState::Error(message) => div().child(message.clone()),
            FormState::Idle | FormState::Pending => div(),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .p_8()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "enter" {
                    this.create_vault(window, cx);
                }
            }))
            .child(CANNOT_RECOVER_PASSWORD_NOTICE)
            .child(
                div()
                    .id("create-vault-password")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child("Password")
                    .child(Input::new(&self.create_password).mask_toggle()),
            )
            .child(
                div()
                    .id("create-vault-confirm")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child("Confirm password")
                    .child(Input::new(&self.create_confirm).mask_toggle()),
            )
            .child(error)
            .child(
                Button::new("create-vault-submit")
                    .label(if pending {
                        "Creating…"
                    } else {
                        "Create Vault"
                    })
                    .disabled(pending || backup_busy)
                    .loading(pending)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.create_vault(window, cx);
                    })),
            )
            .child(self.render_backup_actions(window, cx))
    }

    fn render_locked(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pending = self.unlock_state == FormState::Pending;
        let backup_busy = !self.backup.is_idle();
        let error = match &self.unlock_state {
            FormState::Error(message) => div().child(message.clone()),
            FormState::Idle | FormState::Pending => div(),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .p_8()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "enter" {
                    this.unlock_vault(window, cx);
                }
            }))
            .child(
                div()
                    .id("unlock-password")
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child("Password")
                    .child(Input::new(&self.unlock_password).mask_toggle()),
            )
            .child(error)
            .child(
                Button::new("unlock-submit")
                    .label(if pending { "Unlocking…" } else { "Unlock" })
                    .disabled(pending || backup_busy)
                    .loading(pending)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.unlock_vault(window, cx);
                    })),
            )
            .child(self.render_backup_actions(window, cx))
    }

    fn render_unlocked(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let locker = cx.entity();
        let list = self.render_vault_list(window, cx);
        let editor = self.render_item_editor(locker, window, cx);
        let conflict_panel = if self.conflicts_open {
            self.render_conflicts(window, cx)
        } else {
            div().into_any_element()
        };
        let conflict_count = self.conflicts.count();
        div()
            .size_full()
            .flex()
            .gap_2()
            .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, _, cx| {
                this.note_activity(cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.note_activity(cx);
                }),
            )
            .on_key_down(cx.listener(|this, _: &KeyDownEvent, _, cx| {
                this.note_activity(cx);
            }))
            .child(list)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .child(
                        Button::new("conflicts-button")
                            .label(format!("Conflicts ({conflict_count})"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_conflicts(window, cx)),
                            ),
                    )
                    .child(conflict_panel)
                    .child(editor)
                    .child(self.render_backup_actions(window, cx))
                    .child(
                        Button::new("lock-vault")
                            .label("Lock")
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
                            ),
                    ),
            )
    }
}

impl Render for Locker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.state {
            AppState::NoVault => self.render_no_vault(window, cx).into_any_element(),
            AppState::Locked => self.render_locked(window, cx).into_any_element(),
            AppState::Unlocked(_) => self.render_unlocked(window, cx).into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .on_action(cx.listener(|this, _: &OpenCommandPalette, window, cx| {
                this.window_controls
                    .update(cx, |controls, cx| controls.open_palette(window, cx));
            }))
            .child(self.window_controls.clone())
            .child(div().flex_1().child(body))
            .children(Root::render_dialog_layer(window, cx))
    }
}

/// Fatal startup view used when the platform data path cannot be resolved.
pub struct FatalStartupError {
    message: SharedString,
}

impl FatalStartupError {
    pub fn new(error: VaultError) -> Self {
        eprintln!("could not determine Locker vault path: {error}");
        Self {
            message: "Locker could not determine a safe vault path.".into(),
        }
    }
}

impl Render for FatalStartupError {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        rsx! {
            <div size_full flex items_center justify_center p={px(32.)}>
                {self.message.clone()}
            </div>
        }
    }
}

#[cfg(test)]
mod tests {
    use super::item_editor::EditorMode;
    use super::*;
    use super::{backup, conflicts};
    use gpui::{Focusable, TestAppContext, VisualTestContext};
    use gpui_component::{Root, WindowExt};
    use locker_core::{
        BackupError, ChangeId, ITEM_SCHEMA_VERSION, ItemId, ItemPayload, ItemType, SecretBytes,
    };
    use std::{
        cell::RefCell,
        fs,
        path::Path,
        rc::Rc,
        sync::atomic::{AtomicUsize, Ordering},
    };

    static NEXT_PATH: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn rsx_macro_builds_a_basic_element() {
        let _ = gpui_rsx::rsx! { <div>{"Locker"}</div> };
    }

    fn test_path(label: &str) -> PathBuf {
        let id = NEXT_PATH.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir()
            .join(format!("locker-gui-{label}-{}-{id}", std::process::id()))
            .join("vault.db");
        let _ = fs::remove_dir_all(path.parent().unwrap());
        path
    }

    fn init(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
    }

    fn set_input(view: &Entity<Locker>, cx: &mut VisualTestContext, password: &str) {
        view.update_in(cx, |locker, window, locker_cx| {
            let input = locker.unlock_password.clone();
            input.update(locker_cx, |input, input_cx| {
                input.set_value(password.to_owned(), window, input_cx);
            });
        });
    }

    fn set_create_inputs(
        view: &Entity<Locker>,
        cx: &mut VisualTestContext,
        password: &str,
        confirmation: &str,
    ) {
        view.update_in(cx, |locker, window, locker_cx| {
            let password_input = locker.create_password.clone();
            password_input.update(locker_cx, |input, input_cx| {
                input.set_value(password.to_owned(), window, input_cx);
            });
            let confirmation_input = locker.create_confirm.clone();
            confirmation_input.update(locker_cx, |input, input_cx| {
                input.set_value(confirmation.to_owned(), window, input_cx);
            });
        });
    }

    fn cleanup(path: &Path) {
        let _ = fs::remove_file(path);
        let _ = fs::remove_dir_all(path);
    }

    fn clipboard_text(cx: &mut VisualTestContext) -> Option<String> {
        cx.update(|_, app| app.read_from_clipboard().and_then(|item| item.text()))
    }

    fn clipboard_text_or_empty(cx: &mut VisualTestContext) -> String {
        clipboard_text(cx).unwrap_or_default()
    }

    fn write_existing(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, []).unwrap();
    }

    fn add_locker_view(
        cx: &mut TestAppContext,
        path: PathBuf,
        timeout: Duration,
    ) -> (Entity<Locker>, &mut VisualTestContext) {
        add_locker_view_with_clipboard_timeout(cx, path, timeout, DEFAULT_CLIPBOARD_TIMEOUT)
    }

    fn add_locker_view_with_clipboard_timeout(
        cx: &mut TestAppContext,
        path: PathBuf,
        inactivity_timeout: Duration,
        clipboard_timeout: Duration,
    ) -> (Entity<Locker>, &mut VisualTestContext) {
        let holder = Rc::new(RefCell::new(None));
        let holder_for_window = holder.clone();
        let (_, visual_cx) = cx.add_window_view(move |window, cx| {
            let locker =
                cx.new(|cx| Locker::new(path, inactivity_timeout, clipboard_timeout, window, cx));
            holder_for_window.borrow_mut().replace(locker.clone());
            Root::new(locker, window, cx)
        });
        (holder.take().unwrap(), visual_cx)
    }

    #[gpui::test]
    fn fresh_and_existing_paths_select_the_expected_state(cx: &mut TestAppContext) {
        init(cx);
        let fresh = test_path("fresh");
        let (view, cx) = add_locker_view(cx, fresh.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        assert!(view.read_with(cx, |locker, _| matches!(&locker.state, AppState::NoVault)));
        let input = view.read_with(cx, |locker, _| locker.create_password.clone());
        assert!(cx.update(|window, app| { input.read(app).focus_handle(app).is_focused(window) }));
        cleanup(&fresh);

        let existing = test_path("existing");
        write_existing(&existing);
        let (view, cx) = add_locker_view(cx, existing.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        assert!(view.read_with(cx, |locker, _| matches!(&locker.state, AppState::Locked)));
        let input = view.read_with(cx, |locker, _| locker.unlock_password.clone());
        assert!(cx.update(|window, app| { input.read(app).focus_handle(app).is_focused(window) }));
        cleanup(&existing);
    }

    #[gpui::test]
    fn empty_create_reports_an_error_without_writing_a_vault(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("empty");
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        let before = view.read_with(cx, |locker, _| locker.create_password.entity_id());
        view.update_in(cx, |locker, window, locker_cx| {
            locker.create_vault(window, locker_cx)
        });
        let (state, after) = view.read_with(cx, |locker, _| {
            (
                locker.create_state.clone(),
                locker.create_password.entity_id(),
            )
        });
        assert_eq!(state, FormState::Error("Enter a password.".into()));
        assert_ne!(before, after);
        let input = view.read_with(cx, |locker, _| locker.create_password.clone());
        assert!(cx.update(|window, app| { input.read(app).focus_handle(app).is_focused(window) }));
        assert!(!path.exists());
        cleanup(&path);
    }

    #[gpui::test]
    fn mismatched_create_reports_an_error_and_recreates_inputs(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("mismatch");
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        set_create_inputs(&view, cx, "one", "two");
        let before = view.read_with(cx, |locker, _| locker.create_password.entity_id());
        view.update_in(cx, |locker, window, locker_cx| {
            locker.create_vault(window, locker_cx)
        });
        let (state, after) = view.read_with(cx, |locker, _| {
            (
                locker.create_state.clone(),
                locker.create_password.entity_id(),
            )
        });
        assert_eq!(state, FormState::Error("Passwords do not match.".into()));
        assert_ne!(before, after);
        let input = view.read_with(cx, |locker, _| locker.create_password.clone());
        assert!(cx.update(|window, app| { input.read(app).focus_handle(app).is_focused(window) }));
        assert!(!path.exists());
        cleanup(&path);
    }

    #[gpui::test]
    fn valid_create_keeps_task_until_completion_and_unlocks(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("create");
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        set_create_inputs(&view, cx, "correct horse", "correct horse");
        let before = view.read_with(cx, |locker, _| locker.create_password.entity_id());
        view.update_in(cx, |locker, window, locker_cx| {
            locker.create_vault(window, locker_cx)
        });
        assert_eq!(
            view.read_with(cx, |locker, _| locker.create_state.clone()),
            FormState::Pending
        );
        cx.run_until_parked();
        assert!(view.read_with(cx, |locker, _| matches!(
            &locker.state,
            AppState::Unlocked(_)
        )));
        assert_eq!(
            view.read_with(cx, |locker, _| locker.create_state.clone()),
            FormState::Idle
        );
        assert_ne!(
            before,
            view.read_with(cx, |locker, _| locker.create_password.entity_id())
        );
        assert!(path.exists());
        cleanup(&path);
    }

    #[gpui::test]
    fn wrong_unlock_recreates_the_input_and_uses_the_generic_error(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("wrong-unlock");
        let created = Vault::create(b"correct", &path).unwrap();
        created.lock();
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        let before = view.read_with(cx, |locker, _| locker.unlock_password.entity_id());
        set_input(&view, cx, "wrong");
        view.update_in(cx, |locker, window, locker_cx| {
            locker.unlock_vault(window, locker_cx)
        });
        assert_ne!(
            before,
            view.read_with(cx, |locker, _| locker.unlock_password.entity_id())
        );
        assert_eq!(
            view.read_with(cx, |locker, _| locker.unlock_state.clone()),
            FormState::Pending
        );
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |locker, _| locker.unlock_state.clone()),
            FormState::Error(UNLOCK_ERROR_MESSAGE.into())
        );
        let input = view.read_with(cx, |locker, _| locker.unlock_password.clone());
        assert!(cx.update(|window, app| { input.read(app).focus_handle(app).is_focused(window) }));
        cleanup(&path);
    }

    #[gpui::test]
    fn correct_unlock_restores_the_vault(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("correct-unlock");
        let created = Vault::create(b"correct", &path).unwrap();
        let vault_id = created.vault_id();
        created.lock();
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        set_input(&view, cx, "correct");
        view.update_in(cx, |locker, window, locker_cx| {
            locker.unlock_vault(window, locker_cx)
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |locker, _| {
            matches!(&locker.state, AppState::Unlocked(vault) if vault.vault_id() == vault_id)
        }));
        cleanup(&path);
    }

    #[gpui::test]
    fn duplicate_unlock_while_pending_is_a_no_op(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("pending");
        write_existing(&path);
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        set_input(&view, cx, "wrong");
        view.update_in(cx, |locker, window, locker_cx| {
            locker.unlock_vault(window, locker_cx)
        });
        let first = view.read_with(cx, |locker, _| {
            (
                locker.unlock_state.clone(),
                locker.unlock_password.entity_id(),
            )
        });
        view.update_in(cx, |locker, window, locker_cx| {
            locker.unlock_vault(window, locker_cx)
        });
        let second = view.read_with(cx, |locker, _| {
            (
                locker.unlock_state.clone(),
                locker.unlock_password.entity_id(),
            )
        });
        assert_eq!(first, second);
        cx.run_until_parked();
        cleanup(&path);
    }

    #[gpui::test]
    fn inactivity_locks_and_activity_rearms_one_loop(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("inactivity");
        let vault = Vault::create(b"correct", &path).unwrap();
        let (view, cx) = add_locker_view(cx, path.clone(), Duration::from_secs(60));
        view.update_in(cx, |locker, window, locker_cx| {
            locker.state = AppState::Unlocked(vault);
            locker.arm_inactivity_timer(window, locker_cx);
        });
        assert_eq!(view.read_with(cx, |locker, _| locker.inactivity_epoch), 1);
        cx.executor().advance_clock(Duration::from_secs(60));
        cx.run_until_parked();
        assert!(view.read_with(cx, |locker, _| matches!(&locker.state, AppState::Locked)));

        let second_path = test_path("inactivity-rearm");
        let second_vault = Vault::create(b"correct", &second_path).unwrap();
        view.update_in(cx, |locker, window, locker_cx| {
            locker.state = AppState::Unlocked(second_vault);
            locker.arm_inactivity_timer(window, locker_cx);
        });
        assert_eq!(view.read_with(cx, |locker, _| locker.inactivity_epoch), 3);
        cx.executor().advance_clock(Duration::from_secs(59));
        cx.run_until_parked();
        view.update(cx, |locker, cx| {
            for _ in 0..100 {
                locker.note_activity(cx);
            }
        });
        assert_eq!(view.read_with(cx, |locker, _| locker.inactivity_epoch), 3);
        assert!(view.read_with(cx, |locker, _| matches!(
            &locker.state,
            AppState::Unlocked(_)
        )));
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert!(view.read_with(cx, |locker, _| matches!(
            &locker.state,
            AppState::Unlocked(_)
        )));

        cx.executor().advance_clock(Duration::from_secs(59));
        cx.run_until_parked();
        assert!(view.read_with(cx, |locker, _| matches!(&locker.state, AppState::Locked)));
        cleanup(&path);
        cleanup(&second_path);
    }

    #[gpui::test]
    fn locking_a_no_vault_is_a_no_op(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("no-op-lock");
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.lock_vault(window, locker_cx)
        });
        assert!(view.read_with(cx, |locker, _| matches!(&locker.state, AppState::NoVault)));
        assert_eq!(view.read_with(cx, |locker, _| locker.inactivity_epoch), 0);
        cleanup(&path);
    }

    #[gpui::test]
    fn locking_a_locked_vault_is_a_no_op(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("locked-no-op");
        write_existing(&path);
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        let before_epoch = view.read_with(cx, |locker, _| locker.inactivity_epoch);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.lock_vault(window, locker_cx)
        });
        assert!(view.read_with(cx, |locker, _| matches!(&locker.state, AppState::Locked)));
        assert_eq!(
            view.read_with(cx, |locker, _| locker.inactivity_epoch),
            before_epoch
        );
        cleanup(&path);
    }

    #[gpui::test]
    fn initial_focus_and_enter_submit_are_available_on_the_primary_form(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("focus");
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        let input = view.read_with(cx, |locker, _| locker.create_password.clone());
        assert!(cx.update(|window, app| { input.read(app).focus_handle(app).is_focused(window) }));
        cx.simulate_keystrokes("enter");
        assert_eq!(
            view.read_with(cx, |locker, _| locker.create_state.clone()),
            FormState::Error("Enter a password.".into())
        );
        cleanup(&path);
    }

    #[gpui::test]
    fn ctrl_p_and_command_control_open_the_shared_palette_without_modifying_vault_list_search_state(
        cx: &mut TestAppContext,
    ) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "command-palette", &[]);
        let list_search = view.read_with(cx, |locker, _| {
            locker.vault_list.as_ref().unwrap().search_input.clone()
        });
        view.update_in(cx, |_, window, locker_cx| {
            list_search.update(locker_cx, |input, input_cx| input.focus(window, input_cx));
        });
        let before = cx.update(|_, app| list_search.read(app).value());

        let trigger_bounds = cx.debug_bounds("window-command-palette-trigger").unwrap();
        cx.simulate_click(trigger_bounds.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        assert!(cx.update(|window, app| window.has_active_dialog(app)));
        assert!(cx.debug_bounds("window-command-palette-dialog").is_some());
        assert!(view.read_with(cx, |locker, app| {
            locker.window_controls.read(app).palette_open
        }));

        cx.update(|window, app| window.close_dialog(app));
        view.update(cx, |locker, cx| {
            locker.window_controls.update(cx, |controls, cx| {
                controls.palette_open = false;
                controls.prior_focus = None;
                cx.notify();
            });
        });
        cx.simulate_keystrokes("ctrl-k");
        assert!(!view.read_with(cx, |locker, app| {
            locker.window_controls.read(app).palette_open
        }));

        cx.simulate_keystrokes("ctrl-p");
        cx.run_until_parked();

        assert!(cx.update(|window, app| window.has_active_dialog(app)));
        assert!(cx.debug_bounds("window-command-palette-dialog").is_some());
        assert!(view.read_with(cx, |locker, app| {
            locker.window_controls.read(app).palette_open
        }));
        assert_eq!(cx.update(|_, app| list_search.read(app).value()), before);
        cleanup(&path);
    }

    #[gpui::test]
    fn closing_the_palette_restores_prior_focus(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "command-palette-escape", &[]);
        let list_search = view.read_with(cx, |locker, _| {
            locker.vault_list.as_ref().unwrap().search_input.clone()
        });
        view.update_in(cx, |_, window, locker_cx| {
            list_search.update(locker_cx, |input, input_cx| input.focus(window, input_cx));
        });

        cx.simulate_keystrokes("ctrl-p");
        assert!(view.read_with(cx, |locker, app| {
            locker.window_controls.read(app).palette_open
        }));

        cx.update(|window, app| window.close_dialog(app));
        view.update(cx, |locker, cx| {
            locker.window_controls.update(cx, |controls, cx| {
                controls.palette_open = false;
                controls.prior_focus = None;
                cx.notify();
            });
        });
        assert!(!view.read_with(cx, |locker, app| {
            locker.window_controls.read(app).palette_open
        }));
        assert!(
            cx.update(|window, app| { list_search.read(app).focus_handle(app).is_focused(window) })
        );
        cleanup(&path);
    }

    #[gpui::test]
    fn close_window_command_removes_the_test_window(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("command-close");
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.run_window_command(WindowCommand::Close, window, locker_cx);
        });
        assert!(cx.windows().is_empty());
        cleanup(&path);
    }

    #[gpui::test]
    fn task6_list_load_keeps_items_and_deleted_ids(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("task6-list-load");
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        let item_id = ItemId::new();
        let payload = ItemPayload {
            schema_version: ITEM_SCHEMA_VERSION,
            item_type: ItemType::Login,
            title: "Example".into(),
            username: "alice".into(),
            password: "secret".into(),
            uris: vec![],
            notes: String::new(),
            created_at: 1,
            updated_at: 1,
        };
        view.update_in(cx, |locker, window, locker_cx| {
            locker.vault_list = Some(VaultListState::from_initial_load(
                Ok(vec![(item_id, payload)]),
                Ok(vec![ItemId::new()]),
                window,
                locker_cx,
            ));
        });
        let (items, filtered, deleted) = view.read_with(cx, |locker, _| {
            let list = locker.vault_list.as_ref().unwrap();
            (
                list.items.len(),
                list.filtered.len(),
                list.deleted_ids.len(),
            )
        });
        assert_eq!((items, filtered, deleted), (1, 1, 1));
        cleanup(&path);
    }

    fn login_payload(title: &str, username: &str) -> ItemPayload {
        ItemPayload {
            schema_version: ITEM_SCHEMA_VERSION,
            item_type: ItemType::Login,
            title: title.into(),
            username: username.into(),
            password: "secret".into(),
            uris: vec!["https://example.test".into()],
            notes: "notes".into(),
            created_at: 1,
            updated_at: 1,
        }
    }

    fn unlocked_view<'a>(
        cx: &'a mut TestAppContext,
        label: &str,
        payloads: &[ItemPayload],
    ) -> (
        Entity<Locker>,
        &'a mut VisualTestContext,
        PathBuf,
        Vec<ItemId>,
    ) {
        let path = test_path(label);
        let mut vault = Vault::create(b"correct", &path).unwrap();
        let ids = payloads
            .iter()
            .map(|payload| vault.create_item(payload).unwrap())
            .collect::<Vec<_>>();
        let items = vault.list_items().unwrap();
        let (view, visual_cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        view.update_in(visual_cx, |locker, window, locker_cx| {
            locker.state = AppState::Unlocked(vault);
            locker.vault_list = Some(VaultListState::from_initial_load(
                Ok(items),
                Ok(Vec::new()),
                window,
                locker_cx,
            ));
        });
        (view, visual_cx, path, ids)
    }

    fn set_editor_values(
        view: &Entity<Locker>,
        cx: &mut VisualTestContext,
        title: &str,
        username: &str,
        password: &str,
    ) {
        view.update_in(cx, |locker, window, locker_cx| {
            let editor = locker.item_editor.as_ref().unwrap();
            for (input, value) in [
                (&editor.title_input, title),
                (&editor.username_input, username),
                (&editor.password_input, password),
            ] {
                input.update(locker_cx, |state, input_cx| {
                    state.set_value(value.to_owned(), window, input_cx)
                });
            }
        });
    }

    #[gpui::test]
    fn list_load_failure_is_safe_in_either_branch(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("list-failure");
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.vault_list = Some(VaultListState::from_initial_load(
                Err(VaultError::ItemNotFound),
                Ok(Vec::new()),
                window,
                locker_cx,
            ));
        });
        assert!(view.read_with(cx, |locker, _| {
            matches!(
                locker.vault_list.as_ref().unwrap().load,
                vault_list::ListLoadState::Failed(_)
            ) && locker.vault_list.as_ref().unwrap().items.is_empty()
        }));
        view.update_in(cx, |locker, window, locker_cx| {
            locker.vault_list = Some(VaultListState::from_initial_load(
                Ok(Vec::new()),
                Err(VaultError::ItemNotFound),
                window,
                locker_cx,
            ));
        });
        assert!(view.read_with(cx, |locker, _| {
            matches!(
                locker.vault_list.as_ref().unwrap().load,
                vault_list::ListLoadState::Failed(_)
            ) && locker.vault_list.as_ref().unwrap().filtered.is_empty()
        }));
        cleanup(&path);
    }

    #[gpui::test]
    fn search_change_subscription_filters_and_clears(cx: &mut TestAppContext) {
        init(cx);
        let payloads = [
            login_payload("Alpha", "alice"),
            login_payload("Beta", "bob"),
        ];
        let (view, cx, path, _) = unlocked_view(cx, "search", &payloads);
        let search = view.read_with(cx, |locker, _| {
            locker.vault_list.as_ref().unwrap().search_input.clone()
        });
        view.update_in(cx, |_, window, locker_cx| {
            search.update(locker_cx, |input, input_cx| input.focus(window, input_cx));
        });
        cx.simulate_keystrokes("alp");
        assert_eq!(
            view.read_with(cx, |locker, _| locker
                .vault_list
                .as_ref()
                .unwrap()
                .filtered
                .len()),
            1
        );
        cx.simulate_keystrokes("backspace backspace backspace");
        assert_eq!(
            view.read_with(cx, |locker, _| locker
                .vault_list
                .as_ref()
                .unwrap()
                .filtered
                .len()),
            2
        );
        cleanup(&path);
    }

    #[gpui::test]
    fn create_save_updates_cache_and_persists(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "create-item", &[]);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_create_editor(window, locker_cx)
        });
        set_editor_values(&view, cx, "New", "alice", "new-secret");
        view.update_in(cx, |locker, window, locker_cx| {
            locker.save_item(window, locker_cx)
        });
        let (id, persisted) = view.read_with(cx, |locker, _| {
            let list = locker.vault_list.as_ref().unwrap();
            let (id, _) = list.items.first().unwrap();
            let persisted = match &locker.state {
                AppState::Unlocked(vault) => vault.get_item(*id).unwrap(),
                _ => None,
            };
            (*id, persisted)
        });
        assert_eq!(
            view.read_with(cx, |locker, _| locker.vault_list.as_ref().unwrap().selected),
            Some(id)
        );
        assert_eq!(persisted.unwrap().title, "New");
        cleanup(&path);
    }

    #[gpui::test]
    fn edit_and_restore_round_trip_updates_deleted_projection(cx: &mut TestAppContext) {
        init(cx);
        let payload = login_payload("Original", "alice");
        let (view, cx, path, ids) = unlocked_view(cx, "edit-restore", &[payload]);
        let item_id = ids[0];
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_editor_for_item(item_id, false, window, locker_cx);
        });
        set_editor_values(&view, cx, "Edited", "alice", "changed");
        view.update_in(cx, |locker, window, locker_cx| {
            locker.save_item(window, locker_cx)
        });
        assert_eq!(
            view.read_with(cx, |locker, _| locker.vault_list.as_ref().unwrap().items[0]
                .1
                .title
                .clone()),
            "Edited"
        );
        view.update_in(cx, |locker, window, locker_cx| {
            locker.delete_item(item_id, window, locker_cx)
        });
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_editor_for_item(item_id, true, window, locker_cx)
        });
        assert_eq!(
            view.read_with(cx, |locker, _| locker.item_editor.as_ref().unwrap().mode),
            EditorMode::Restore(item_id)
        );
        view.update_in(cx, |locker, window, locker_cx| {
            locker.save_item(window, locker_cx)
        });
        assert!(view.read_with(cx, |locker, _| {
            locker.vault_list.as_ref().unwrap().deleted_ids.is_empty()
        }));
        assert!(view.read_with(cx, |locker, _| matches!(&locker.state, AppState::Unlocked(vault) if vault.get_item(item_id).unwrap().is_some())));
        cleanup(&path);
    }

    #[gpui::test]
    fn direct_delete_updates_cache_and_closes_editor(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, ids) =
            unlocked_view(cx, "delete-item", &[login_payload("Delete", "alice")]);
        let item_id = ids[0];
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_editor_for_item(item_id, false, window, locker_cx)
        });
        view.update_in(cx, |locker, window, locker_cx| {
            locker.delete_item(item_id, window, locker_cx)
        });
        assert!(view.read_with(cx, |locker, _| {
            let list = locker.vault_list.as_ref().unwrap();
            list.items.is_empty()
                && list.filtered.is_empty()
                && list.deleted_ids == [item_id]
                && locker.item_editor.is_none()
        }));
        cleanup(&path);
    }

    #[gpui::test]
    fn deleted_id_bookkeeping_supports_multiple_delete_and_restore(cx: &mut TestAppContext) {
        init(cx);
        let payloads = [login_payload("One", "one"), login_payload("Two", "two")];
        let (view, cx, path, ids) = unlocked_view(cx, "deleted-bookkeeping", &payloads);
        for item_id in ids.iter().copied() {
            view.update_in(cx, |locker, window, locker_cx| {
                locker.delete_item(item_id, window, locker_cx)
            });
        }
        assert!(view.read_with(
            cx,
            |locker, _| locker.vault_list.as_ref().unwrap().deleted_ids.len() == 2
        ));
        let restore_id = ids[0];
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_editor_for_item(restore_id, true, window, locker_cx)
        });
        view.update_in(cx, |locker, window, locker_cx| {
            locker.save_item(window, locker_cx)
        });
        assert!(view.read_with(cx, |locker, _| {
            let list = locker.vault_list.as_ref().unwrap();
            list.deleted_ids == [ids[1]] && list.items.iter().any(|(id, _)| *id == restore_id)
        }));
        cleanup(&path);
    }

    #[gpui::test]
    fn restore_mode_is_preserved_through_save(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, ids) =
            unlocked_view(cx, "restore-mode", &[login_payload("Restore", "alice")]);
        let item_id = ids[0];
        view.update_in(cx, |locker, window, locker_cx| {
            locker.delete_item(item_id, window, locker_cx)
        });
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_editor_for_item(item_id, true, window, locker_cx)
        });
        assert_eq!(
            view.read_with(cx, |locker, _| locker.item_editor.as_ref().unwrap().mode),
            EditorMode::Restore(item_id)
        );
        view.update_in(cx, |locker, window, locker_cx| {
            locker.save_item(window, locker_cx)
        });
        assert!(!view.read_with(cx, |locker, _| {
            locker
                .vault_list
                .as_ref()
                .unwrap()
                .deleted_ids
                .contains(&item_id)
        }));
        cleanup(&path);
    }

    #[gpui::test]
    fn clipboard_timer_clears_only_unchanged_text(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("clipboard");
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.copy_secret(SecretBytes::new(b"clipboard-secret"), window, locker_cx);
        });
        assert_eq!(clipboard_text(cx), Some("clipboard-secret".into()));
        cx.executor().advance_clock(DEFAULT_CLIPBOARD_TIMEOUT);
        cx.run_until_parked();
        assert_eq!(clipboard_text_or_empty(cx), String::new());
        assert!(view.read_with(cx, |locker, _| locker.clipboard.expected.is_none()));

        view.update_in(cx, |locker, window, locker_cx| {
            locker.copy_secret(SecretBytes::new(b"keep-me"), window, locker_cx);
        });
        cx.update(|_, app| {
            app.write_to_clipboard(gpui::ClipboardItem::new_string("external".into()))
        });
        cx.executor().advance_clock(DEFAULT_CLIPBOARD_TIMEOUT);
        cx.run_until_parked();
        assert_eq!(clipboard_text(cx), Some("external".into()));
        assert!(view.read_with(cx, |locker, _| locker.clipboard.expected.is_none()));
        cleanup(&path);
    }

    #[gpui::test]
    fn newest_clipboard_copy_owns_the_injected_timeout(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("clipboard-epoch");
        let timeout = Duration::from_secs(1);
        let (view, cx) = add_locker_view_with_clipboard_timeout(
            cx,
            path.clone(),
            DEFAULT_INACTIVITY_TIMEOUT,
            timeout,
        );
        view.update_in(cx, |locker, window, locker_cx| {
            locker.copy_secret(SecretBytes::new(b"first"), window, locker_cx);
            locker.copy_secret(SecretBytes::new(b"second"), window, locker_cx);
        });
        assert_eq!(clipboard_text(cx), Some("second".into()));
        cx.executor().advance_clock(timeout);
        cx.run_until_parked();
        assert_eq!(clipboard_text_or_empty(cx), String::new());
        assert!(view.read_with(cx, |locker, _| locker.clipboard.expected.is_none()));
        cleanup(&path);
    }

    #[gpui::test]
    fn locking_clears_our_clipboard_secret(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "lock-clipboard", &[]);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.copy_secret(SecretBytes::new(b"clipboard-secret"), window, locker_cx);
            locker.lock_vault(window, locker_cx);
        });
        assert_eq!(clipboard_text_or_empty(cx), String::new());
        assert!(view.read_with(cx, |locker, _| {
            matches!(&locker.state, AppState::Locked) && locker.clipboard.expected.is_none()
        }));
        cleanup(&path);
    }

    #[gpui::test]
    fn explicit_lock_clears_unlocked_state_and_focuses_unlock_input(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, ids) =
            unlocked_view(cx, "lock-cleanup", &[login_payload("Lock me", "alice")]);
        let before_epoch = view.read_with(cx, |locker, _| locker.inactivity_epoch);
        let before_backup_epoch = view.read_with(cx, |locker, _| locker.backup.epoch);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.arm_inactivity_timer(window, locker_cx);
            locker.open_editor_for_item(ids[0], false, window, locker_cx);
            locker.conflicts = conflicts::ConflictState::from_initial_load(Ok(vec![(
                ids[0],
                vec![(ChangeId::new(), Some(login_payload("Conflict", "alice")))],
            )]));
            locker.conflicts_open = true;
            locker.backup.operation = backup::BackupOperation::ChoosingExportPath;
            locker.copy_secret(SecretBytes::new(b"clipboard-secret"), window, locker_cx);
            locker.lock_vault(window, locker_cx);
        });
        let unlock_input = view.read_with(cx, |locker, _| locker.unlock_password.clone());
        let (
            state,
            inactivity_epoch,
            backup_epoch,
            backup_idle,
            clipboard,
            list,
            editor,
            conflicts,
            conflicts_open,
        ) = view.read_with(cx, |locker, _| {
            (
                matches!(&locker.state, AppState::Locked),
                locker.inactivity_epoch,
                locker.backup.epoch,
                locker.backup.is_idle(),
                locker.clipboard.expected.is_none(),
                locker.vault_list.is_none(),
                locker.item_editor.is_none(),
                matches!(&locker.conflicts, conflicts::ConflictState::Closed),
                locker.conflicts_open,
            )
        });
        assert!(state);
        assert_eq!(inactivity_epoch, before_epoch + 2);
        assert_eq!(backup_epoch, before_backup_epoch + 1);
        assert!(backup_idle && clipboard && list && editor && conflicts && !conflicts_open);
        assert!(
            cx.update(|window, app| {
                unlock_input.read(app).focus_handle(app).is_focused(window)
            })
        );
        cleanup(&path);
    }

    #[gpui::test]
    fn lock_button_enter_locks_when_focused(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "lock-button-enter", &[]);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.arm_inactivity_timer(window, locker_cx);
        });
        cx.update(|window, _| window.blur());
        for _ in 0..6 {
            cx.update(|window, app| window.focus_next(app));
        }
        cx.simulate_keystrokes("enter");
        assert!(view.read_with(cx, |locker, _| matches!(&locker.state, AppState::Locked)));
        assert_eq!(view.read_with(cx, |locker, _| locker.inactivity_epoch), 2);
        let unlock_input = view.read_with(cx, |locker, _| locker.unlock_password.clone());
        assert!(
            cx.update(|window, app| {
                unlock_input.read(app).focus_handle(app).is_focused(window)
            })
        );
        cleanup(&path);
    }

    #[gpui::test]
    fn lock_button_space_locks_and_other_keys_do_not(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "lock-button-space", &[]);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.arm_inactivity_timer(window, locker_cx);
        });
        cx.update(|window, _| window.blur());
        for _ in 0..6 {
            cx.update(|window, app| window.focus_next(app));
        }
        cx.simulate_keystrokes("a");
        assert!(view.read_with(cx, |locker, _| matches!(
            &locker.state,
            AppState::Unlocked(_)
        )));
        cx.simulate_keystrokes("space");
        assert!(view.read_with(cx, |locker, _| matches!(&locker.state, AppState::Locked)));
        assert_eq!(view.read_with(cx, |locker, _| locker.inactivity_epoch), 2);
        cleanup(&path);
    }

    #[gpui::test]
    fn unfocused_lock_button_key_does_not_lock(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "lock-button-unfocused", &[]);
        cx.update(|window, _| window.blur());
        cx.simulate_keystrokes("space");
        assert!(view.read_with(cx, |locker, _| matches!(
            &locker.state,
            AppState::Unlocked(_)
        )));
        cleanup(&path);
    }

    #[gpui::test]
    fn conflicts_preserve_change_ids_and_tombstones_without_selection(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("conflict-model");
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        let item_id = ItemId::new();
        let edit_id = ChangeId::new();
        let delete_id = ChangeId::new();
        let payload = login_payload("Conflict", "alice");
        view.update_in(cx, |locker, window, cx| {
            locker.conflicts = conflicts::ConflictState::from_initial_load(Ok(vec![(
                item_id,
                vec![(edit_id, Some(payload)), (delete_id, None)],
            )]));
            locker.open_conflicts(window, cx);
        });
        let (selected, ids, has_delete) = view.read_with(cx, |locker, _| {
            let conflicts::ConflictState::Ready(items) = &locker.conflicts else {
                panic!("expected ready conflicts")
            };
            (
                items[0].selected,
                items[0]
                    .choices
                    .iter()
                    .map(|choice| choice.change_id)
                    .collect::<Vec<_>>(),
                items[0]
                    .choices
                    .iter()
                    .any(|choice| choice.payload.is_none()),
            )
        });
        assert_eq!(selected, None);
        assert_eq!(ids, vec![edit_id, delete_id]);
        assert!(has_delete);
        cleanup(&path);
    }

    #[gpui::test]
    fn stale_backup_completion_and_error_messages_are_safe(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("backup-state");
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.backup.operation = backup::BackupOperation::ChoosingExportPath;
            locker.backup.epoch = 2;
            locker.finish_export_path_selection(
                1,
                Ok(Some(path.with_extension("lockbak"))),
                window,
                locker_cx,
            );
        });
        assert!(view.read_with(cx, |locker, _| {
            matches!(
                locker.backup.operation,
                backup::BackupOperation::ChoosingExportPath
            ) && locker.backup.dialog.is_none()
        }));
        let error = backup::backup_error_message("restore", &BackupError::AuthenticationFailed);
        assert!(!error.contains("backup-password"));
        cleanup(&path);
    }

    #[gpui::test]
    fn cancelled_backup_pickers_and_failed_fresh_restore_reset_safely(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("backup-cancel-focus");
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.backup.operation = backup::BackupOperation::ChoosingExportPath;
            locker.backup.epoch = 1;
            locker.finish_export_path_selection(1, Ok(None), window, locker_cx);
            assert!(matches!(
                locker.backup.operation,
                backup::BackupOperation::Idle
            ));

            locker.backup.operation = backup::BackupOperation::ChoosingRestorePath;
            locker.backup.epoch = 2;
            locker.finish_restore_path_selection(2, Ok(None), window, locker_cx);
            assert!(matches!(
                locker.backup.operation,
                backup::BackupOperation::Idle
            ));

            locker.backup.operation = backup::BackupOperation::Restoring;
            locker.backup.epoch = 3;
            locker.finish_restore(
                3,
                Err(BackupError::AuthenticationFailed),
                true,
                window,
                locker_cx,
            );
        });
        let input = view.read_with(cx, |locker, _| locker.create_password.clone());
        assert!(view.read_with(cx, |locker, _| matches!(&locker.state, AppState::NoVault)));
        assert!(cx.update(|window, app| { input.read(app).focus_handle(app).is_focused(window) }));
        cleanup(&path);
    }
}
