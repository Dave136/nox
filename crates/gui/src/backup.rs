use crate::app::{AppState, Nox, animated_auth_button};
use crate::theme::Theme;
use gpui::{
    Animation, AnimationExt, AnyElement, Context, Entity, FocusHandle, FontWeight, KeyDownEvent,
    PathPromptOptions, SharedString, Task, Window, div, ease_out_quint, prelude::*, px,
};
use gpui_component::{
    Disableable, ThemeMode, WindowExt,
    button::Button,
    input::{Input, InputState},
};
use gpui_rsx::rsx;
use nox_core::{
    BackupError, MAX_BACKUP_PASSWORD_BYTES, RestoreResult, SecretBytes, restore_from_path,
};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use crate::assets::logo;

const RESTORE_TRANSITION_DURATION: Duration = Duration::from_millis(180);

pub(crate) enum BackupOperation {
    Idle,
    ChoosingExportPath,
    Exporting,
    ChoosingRestorePath,
    AwaitingRestoreConfirmation { archive_path: PathBuf },
    Restoring,
    Succeeded(SharedString),
    Failed(SharedString),
}

pub(crate) enum BackupDialogState {
    Export {
        destination: PathBuf,
        password: Entity<InputState>,
        confirmation: Entity<InputState>,
    },
    Restore {
        backup_password: Entity<InputState>,
        new_master_password: Entity<InputState>,
        new_master_confirmation: Entity<InputState>,
        archive_path: PathBuf,
    },
}

pub(crate) struct BackupState {
    pub(crate) operation: BackupOperation,
    pub(crate) dialog: Option<BackupDialogState>,
    pub(crate) epoch: u64,
    pub(crate) task: Task<()>,
    pub(crate) restore_error: Option<SharedString>,
    pub(crate) restore_exiting: bool,
    pub(crate) transition_task: Task<()>,
    pub(crate) restore_prior_focus: Option<FocusHandle>,
    pub(crate) restore_reopen_picker: bool,
}

impl BackupState {
    pub(crate) fn new() -> Self {
        Self {
            operation: BackupOperation::Idle,
            dialog: None,
            epoch: 0,
            task: Task::ready(()),
            restore_error: None,
            restore_exiting: false,
            transition_task: Task::ready(()),
            restore_prior_focus: None,
            restore_reopen_picker: false,
        }
    }

    pub(crate) fn is_idle(&self) -> bool {
        matches!(
            self.operation,
            BackupOperation::Idle | BackupOperation::Succeeded(_) | BackupOperation::Failed(_)
        )
    }
}

fn masked_input(
    window: &mut Window,
    cx: &mut Context<Nox>,
    placeholder: &'static str,
) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .masked(true)
            .placeholder(placeholder)
    })
}

impl Nox {
    pub(crate) fn leave_restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(
            self.backup.operation,
            BackupOperation::AwaitingRestoreConfirmation { .. }
        ) || self.backup.restore_exiting
        {
            return;
        }
        if cx.reduce_motion() {
            self.finish_restore_exit(window, cx);
            return;
        }
        self.backup.restore_exiting = true;
        let epoch = self.backup.epoch;
        self.backup.transition_task = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(RESTORE_TRANSITION_DURATION)
                .await;
            if let Some(this) = this.upgrade() {
                let _ = cx.update(|window, app| {
                    this.update(app, |this, cx| {
                        if this.backup.epoch == epoch && this.backup.restore_exiting {
                            this.finish_restore_exit(window, cx);
                        }
                    });
                });
            }
        });
        cx.notify();
    }

    pub(crate) fn choose_different_restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.backup.restore_reopen_picker = true;
        self.leave_restore(window, cx);
    }

    fn finish_restore_exit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let reopen_picker = self.backup.restore_reopen_picker;
        self.backup.epoch = self.backup.epoch.wrapping_add(1);
        self.backup.operation = BackupOperation::Idle;
        self.backup.dialog = None;
        self.backup.restore_error = None;
        self.backup.restore_exiting = false;
        self.backup.restore_reopen_picker = false;
        let mode = match self.state {
            AppState::NoVault | AppState::RegistryError | AppState::Locked => ThemeMode::Dark,
            AppState::Unlocked(_) => ThemeMode::Light,
        };
        crate::theme::apply(mode, Some(window), cx);
        if let Some(focus) = self.backup.restore_prior_focus.take() {
            focus.focus(window, cx);
        }
        cx.notify();
        if reopen_picker {
            self.begin_restore(window, cx);
        }
    }

    pub(crate) fn begin_export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.state, AppState::Unlocked(_)) || !self.backup.is_idle() {
            return;
        }
        let Some(directory) = self.vault_path().map(|path| {
            path.parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf()
        }) else {
            return;
        };
        self.backup.epoch = self.backup.epoch.wrapping_add(1);
        let epoch = self.backup.epoch;
        self.backup.operation = BackupOperation::ChoosingExportPath;
        self.backup.dialog = None;
        let receiver = cx.prompt_for_new_path(&directory, Some("nox-backup.lockbak"));
        self.backup.task = cx.spawn_in(window, async move |this, cx| {
            let result = receiver
                .await
                .map_err(|_| ())
                .and_then(|result| result.map_err(|_| ()));
            if let Some(this) = this.upgrade() {
                let _ = cx.update(|window, app| {
                    this.update(app, |this, cx| {
                        this.finish_export_path_selection(epoch, result, window, cx)
                    });
                });
            }
        });
        self.note_activity(cx);
        cx.notify();
    }

    pub(crate) fn finish_export_path_selection(
        &mut self,
        epoch: u64,
        result: Result<Option<PathBuf>, ()>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.backup.epoch != epoch
            || !matches!(self.backup.operation, BackupOperation::ChoosingExportPath)
        {
            return;
        }
        let destination = match result {
            Ok(Some(destination)) => destination,
            Ok(None) => {
                self.backup.operation = BackupOperation::Idle;
                self.backup.task = Task::ready(());
                cx.notify();
                return;
            }
            Err(()) => {
                self.backup.operation =
                    BackupOperation::Failed("Could not open the file picker.".into());
                self.backup.task = Task::ready(());
                cx.notify();
                return;
            }
        };
        let password = masked_input(window, cx, "Backup password");
        let confirmation = masked_input(window, cx, "Confirm backup password");
        self.backup.dialog = Some(BackupDialogState::Export {
            destination: destination.clone(),
            password: password.clone(),
            confirmation: confirmation.clone(),
        });
        let locker = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |dialog, _window, _cx| {
            let ok_locker = locker.clone();
            let cancel_locker = locker.clone();
            dialog
                .title("Export backup")
                .child(format!("Destination: {}", destination.display()))
                .child("Nox cannot recover this backup password. Without it, the backup cannot be restored.")
                .child(Input::new(&password).mask_toggle())
                .child(Input::new(&confirmation).mask_toggle())
                .confirm()
                .on_ok(move |_, window, app| {
                    let mut accepted = false;
                    let _ = ok_locker.update(app, |locker, cx| {
                        accepted = locker.submit_export_password(window, cx);
                    });
                    accepted
                })
                .on_cancel(move |_, _, app| {
                        let _ = cancel_locker.update(app, |locker, cx| locker.cancel_backup_dialog(cx));
                        true
                })
        });
    }

    pub(crate) fn submit_export_password(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(BackupDialogState::Export {
            destination,
            password: password_input,
            confirmation: confirmation_input,
        }) = self.backup.dialog.take()
        else {
            return false;
        };
        let password = SecretBytes::new(password_input.read(cx).value().as_bytes());
        let confirmation = SecretBytes::new(confirmation_input.read(cx).value().as_bytes());
        if password.is_empty()
            || password.as_bytes() != confirmation.as_bytes()
            || password.len() > MAX_BACKUP_PASSWORD_BYTES
        {
            self.backup.dialog = Some(BackupDialogState::Export {
                destination,
                password: password_input,
                confirmation: confirmation_input,
            });
            return false;
        }
        let request = match &self.state {
            AppState::Unlocked(session) => session
                .vault
                .prepare_backup_export(password.as_bytes(), &destination),
            _ => return false,
        };
        let request = match request {
            Ok(request) => request,
            Err(error) => {
                self.backup.operation =
                    BackupOperation::Failed(backup_error_message("export", &error));
                cx.notify();
                return true;
            }
        };
        self.backup.epoch = self.backup.epoch.wrapping_add(1);
        let epoch = self.backup.epoch;
        self.backup.operation = BackupOperation::Exporting;
        self.backup.task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { request.run() })
                .await;
            if let Some(this) = this.upgrade() {
                let _ = cx.update(|_, app| {
                    this.update(app, |this, cx| this.finish_export(epoch, result, cx));
                });
            }
        });
        cx.notify();
        true
    }

    pub(crate) fn cancel_backup_dialog(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.backup.operation,
            BackupOperation::ChoosingExportPath
                | BackupOperation::AwaitingRestoreConfirmation { .. }
        ) {
            self.backup.epoch = self.backup.epoch.wrapping_add(1);
            self.backup.operation = BackupOperation::Idle;
            self.backup.dialog = None;
            self.backup.task = Task::ready(());
            cx.notify();
        }
    }

    pub(crate) fn finish_export(
        &mut self,
        epoch: u64,
        result: Result<(), BackupError>,
        cx: &mut Context<Self>,
    ) {
        if self.backup.epoch != epoch
            || !matches!(self.backup.operation, BackupOperation::Exporting)
        {
            return;
        }
        self.backup.operation = match result {
            Ok(()) => BackupOperation::Succeeded("Backup exported.".into()),
            Err(error) => BackupOperation::Failed(backup_error_message("export", &error)),
        };
        self.backup.task = Task::ready(());
        cx.notify();
    }

    pub(crate) fn begin_restore(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.backup.is_idle() {
            return;
        }
        self.backup.epoch = self.backup.epoch.wrapping_add(1);
        let epoch = self.backup.epoch;
        self.backup.operation = BackupOperation::ChoosingRestorePath;
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose Nox backup".into()),
        });
        self.backup.task = cx.spawn_in(window, async move |this, cx| {
            let result = receiver
                .await
                .map_err(|_| ())
                .and_then(|result| result.map_err(|_| ()));
            if let Some(this) = this.upgrade() {
                let _ = cx.update(|window, app| {
                    this.update(app, |this, cx| {
                        this.finish_restore_path_selection(epoch, result, window, cx)
                    });
                });
            }
        });
        self.note_activity(cx);
        cx.notify();
    }

    pub(crate) fn finish_restore_path_selection(
        &mut self,
        epoch: u64,
        result: Result<Option<Vec<PathBuf>>, ()>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.backup.epoch != epoch
            || !matches!(self.backup.operation, BackupOperation::ChoosingRestorePath)
        {
            return;
        }
        let paths = match result {
            Ok(Some(paths)) => paths,
            Ok(None) => {
                self.backup.operation = BackupOperation::Idle;
                self.backup.task = Task::ready(());
                cx.notify();
                return;
            }
            Err(()) => {
                self.backup.operation =
                    BackupOperation::Failed("Could not open the file picker.".into());
                self.backup.task = Task::ready(());
                cx.notify();
                return;
            }
        };
        if paths.len() != 1 {
            self.backup.operation = BackupOperation::Failed("Choose one backup file.".into());
            self.backup.task = Task::ready(());
            cx.notify();
            return;
        }
        let archive_path = paths.into_iter().next().expect("length checked");
        self.request_restore_confirmation(archive_path, window, cx);
    }

    pub(crate) fn request_restore_confirmation(
        &mut self,
        archive_path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let backup_password = masked_input(window, cx, "Enter the backup password");
        let new_master_password = masked_input(window, cx, "Create a new master password");
        let new_master_confirmation = masked_input(window, cx, "Re-enter the new master password");
        self.backup.operation = BackupOperation::AwaitingRestoreConfirmation {
            archive_path: archive_path.clone(),
        };
        self.backup.restore_error = None;
        self.backup.restore_exiting = false;
        self.backup.restore_reopen_picker = false;
        self.backup.restore_prior_focus = window.focused(cx);
        self.backup.transition_task = Task::ready(());
        self.backup.dialog = Some(BackupDialogState::Restore {
            backup_password: backup_password.clone(),
            new_master_password: new_master_password.clone(),
            new_master_confirmation: new_master_confirmation.clone(),
            archive_path: archive_path.clone(),
        });
        crate::theme::apply(ThemeMode::Dark, Some(window), cx);
        Self::focus_input(&backup_password, window, cx);
        cx.notify();
    }

    pub(crate) fn confirm_restore(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.backup.restore_exiting {
            return false;
        }
        let Some(BackupDialogState::Restore {
            backup_password: backup_password_input,
            new_master_password: master_password_input,
            new_master_confirmation: master_confirmation_input,
            archive_path,
        }) = self.backup.dialog.take()
        else {
            return false;
        };
        let backup = SecretBytes::new(backup_password_input.read(cx).value().as_bytes());
        let master = SecretBytes::new(master_password_input.read(cx).value().as_bytes());
        let confirmation = SecretBytes::new(master_confirmation_input.read(cx).value().as_bytes());
        let error = if backup.is_empty() || master.is_empty() || confirmation.is_empty() {
            Some("Enter all three passwords.")
        } else if master.as_bytes() != confirmation.as_bytes() {
            Some("New master passwords do not match.")
        } else if backup.len() > MAX_BACKUP_PASSWORD_BYTES
            || master.len() > MAX_BACKUP_PASSWORD_BYTES
        {
            Some("Password is too long.")
        } else {
            None
        };
        if let Some(error) = error {
            self.backup.restore_error = Some(error.into());
            self.backup.dialog = Some(BackupDialogState::Restore {
                backup_password: backup_password_input,
                new_master_password: master_password_input,
                new_master_confirmation: master_confirmation_input,
                archive_path,
            });
            return false;
        }
        self.backup.restore_error = None;
        self.backup.restore_prior_focus = None;
        self.backup.restore_exiting = false;
        self.backup.transition_task = Task::ready(());
        let was_no_vault = matches!(self.state, AppState::NoVault);
        let destination = match self.vault_path() {
            Some(path) => path.to_path_buf(),
            None if was_no_vault => self.data_dir.join("vault.db"),
            None => return false,
        };
        window.close_all_dialogs(cx);
        self.discard_clipboard_state(cx);
        self.conflicts = crate::conflicts::ConflictState::Closed;
        self.conflicts_open = false;
        if let AppState::Unlocked(session) = std::mem::replace(&mut self.state, AppState::Locked) {
            session.vault.lock();
        }
        self.reset_unlock_input(window, cx);
        self.backup.epoch = self.backup.epoch.wrapping_add(1);
        let epoch = self.backup.epoch;
        self.backup.operation = BackupOperation::Restoring;
        let backup_secret = backup;
        let master_secret = master;
        self.backup.task = cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    restore_from_path(
                        &archive_path,
                        &destination,
                        backup_secret.as_bytes(),
                        master_secret.as_bytes(),
                    )
                })
                .await;
            if let Some(this) = this.upgrade() {
                let _ = cx.update(|window, app| {
                    this.update(app, |this, cx| {
                        this.finish_restore(epoch, result, was_no_vault, window, cx)
                    });
                });
            }
        });
        cx.notify();
        true
    }

    pub(crate) fn finish_restore(
        &mut self,
        epoch: u64,
        result: Result<RestoreResult, BackupError>,
        was_no_vault: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.backup.epoch != epoch
            || !matches!(self.backup.operation, BackupOperation::Restoring)
        {
            return;
        }
        match result {
            Ok(result) => {
                if self.active_vault.is_none() {
                    self.active_vault = Some(crate::vaults::VaultEntry::new(
                        "Personal vault",
                        self.data_dir.join("vault.db"),
                    ));
                }
                self.state = AppState::Locked;
                self.reset_unlock_input(window, cx);
                Self::focus_input(&self.unlock_password, window, cx);
                let identity = if result.fresh_vault {
                    "new vault identity created"
                } else {
                    "vault identity retained"
                };
                self.backup.operation = BackupOperation::Succeeded(
                    format!(
                        "Backup restored: {} changes imported; {}.",
                        result.imported_changes, identity
                    )
                    .into(),
                );
            }
            Err(error) => {
                let destination_exists = self.vault_path().is_some_and(Path::exists)
                    || self.data_dir.join("vault.db").exists();
                if was_no_vault && !destination_exists {
                    self.state = AppState::NoVault;
                    self.reset_create_inputs(window, cx);
                    Self::focus_input(&self.create_password, window, cx);
                } else {
                    self.state = AppState::Locked;
                }
                self.backup.operation =
                    BackupOperation::Failed(backup_error_message("restore", &error));
            }
        }
        let mode = match self.state {
            AppState::NoVault | AppState::RegistryError | AppState::Locked => ThemeMode::Dark,
            AppState::Unlocked(_) => ThemeMode::Light,
        };
        crate::theme::apply(mode, Some(window), cx);
        self.backup.task = Task::ready(());
        cx.notify();
    }

    pub(crate) fn render_restore_backup(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::current(cx);
        let Some(BackupDialogState::Restore {
            backup_password,
            new_master_password,
            new_master_confirmation,
            archive_path,
        }) = &self.backup.dialog
        else {
            return div().into_any_element();
        };
        let backup_password = backup_password.clone();
        let new_master_password = new_master_password.clone();
        let new_master_confirmation = new_master_confirmation.clone();
        let archive_path = archive_path.clone();
        let archive_name = archive_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Selected backup")
            .to_owned();
        let archive_display = archive_path.display().to_string();
        let exiting = self.backup.restore_exiting;
        let error = self
            .backup
            .restore_error
            .clone()
            .map(|message| {
                div()
                    .text_sm()
                    .text_center()
                    .text_color(theme.danger)
                    .child(message)
            })
            .unwrap_or_else(div);
        let restore_button = Button::new("restore-backup-submit")
            .w_full()
            .h(px(44.))
            .rounded(px(7.))
            .disabled(exiting)
            .on_click(cx.listener(|this, _, window, cx| {
                this.confirm_restore(window, cx);
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .font_weight(FontWeight::BOLD)
                    .child(
                        gpui_component::Icon::empty()
                            .path("icons/refresh-cw.svg")
                            .size(px(15.)),
                    )
                    .child("Restore backup"),
            )
            .when(exiting, |button| {
                button
                    .bg(theme.inverse_disabled)
                    .text_color(theme.on_inverse)
            });
        let restore_button = animated_auth_button(
            "restore-backup-submit",
            restore_button,
            self.auth_hovered.get("restore-backup-submit").copied(),
            (
                theme.inverse,
                theme.inverse_hover,
                theme.inverse_press,
                theme.on_inverse,
            ),
            cx,
        );
        let back = Button::new("restore-back")
            .h(px(32.))
            .disabled(exiting)
            .on_click(cx.listener(|this, _, window, cx| this.leave_restore(window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .child(
                        gpui_component::Icon::empty()
                            .path("icons/arrow-left.svg")
                            .size(px(15.)),
                    )
                    .child("Back"),
            );
        let back = animated_auth_button(
            "restore-back",
            back,
            self.auth_hovered.get("restore-back").copied(),
            (
                theme.canvas,
                theme.raised,
                theme.border,
                theme.text_secondary,
            ),
            cx,
        );
        let choose_different = Button::new("restore-choose-different")
            .h(px(32.))
            .disabled(exiting)
            .on_click(cx.listener(|this, _, window, cx| this.choose_different_restore(window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .child(
                        gpui_component::Icon::empty()
                            .path("icons/refresh-cw.svg")
                            .size(px(14.))
                            .text_color(theme.text_subtle),
                    )
                    .child("Choose a different backup"),
            );
        let choose_different = animated_auth_button(
            "restore-choose-different",
            choose_different,
            self.auth_hovered.get("restore-choose-different").copied(),
            (
                theme.canvas,
                theme.raised,
                theme.border,
                theme.text_secondary,
            ),
            cx,
        );
        let page = rsx! {
            <div
                id="restore-backup-view"
                relative
                size_full
                flex
                items_center
                justify_center
                bg={theme.canvas}
                p={px(36.)}
                onKeyDown={cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    match event.keystroke.key.as_str() {
                        "enter" => {
                            window.prevent_default();
                            this.confirm_restore(window, cx);
                        }
                        "escape" => {
                            window.prevent_default();
                            this.leave_restore(window, cx);
                        }
                        _ => {}
                    }
                })}
            >
                <div absolute top={px(20.)} left={px(24.)}>
                    {back}
                </div>
                <div flex flex_col gap={px(13.)} w={px(416.)}>
                    <div flex flex_col items_center gap={px(8.)}>
                        {logo(52., theme.text)}
                        <div flex flex_col items_center gap={px(4.)}>
                            <div text_lg fontWeight={FontWeight::SEMIBOLD} textColor={theme.text}>
                                {"Restore your vault"}
                            </div>
                            <div text_xs text_center textColor={theme.text_muted}>
                                {"Import an encrypted backup and choose a new master password"}
                            </div>
                        </div>
                    </div>
                    <div
                        id="restore-archive-summary"
                        flex
                        items_center
                        gap={px(10.)}
                        h={px(48.)}
                        px={px(12.)}
                        rounded={px(8.)}
                        bg={theme.surface}
                    >
                        <div size={px(28.)} flex items_center justify_center rounded_full bg={theme.raised}>
                            {logo(14., theme.text)}
                        </div>
                        <div flex flex_col flex_1 min_w={px(0.)} gap={px(2.)}>
                            <div text_sm fontWeight={FontWeight::SEMIBOLD} textColor={theme.text_soft}>
                                {archive_name}
                            </div>
                            <div text_xs textColor={theme.text_subtle} overflow_hidden>
                                {archive_display}
                            </div>
                        </div>
                        {gpui_component::Icon::empty()
                            .path("icons/file-lock.svg")
                            .size(px(14.))
                            .text_color(theme.text_subtle)}
                    </div>
                    <div flex flex_col gap={px(7.)}>
                        <div text_xs fontWeight={FontWeight::BOLD} textColor={theme.text_subtle}>
                            {"BACKUP PASSWORD"}
                        </div>
                        <Input
                            base={Input::new(&backup_password)
                                .mask_toggle()
                                .aria_label("Backup password")
                                .disabled(exiting)}
                            h={px(44.)}
                            bg={theme.surface}
                            borderColor={theme.border_strong}
                            rounded={px(8.)}
                        />
                    </div>
                    <div flex flex_col gap={px(7.)}>
                        <div text_xs fontWeight={FontWeight::BOLD} textColor={theme.text_subtle}>
                            {"NEW MASTER PASSWORD"}
                        </div>
                        <Input
                            base={Input::new(&new_master_password)
                                .mask_toggle()
                                .aria_label("New master password")
                                .disabled(exiting)}
                            h={px(44.)}
                            bg={theme.surface}
                            borderColor={theme.border_strong}
                            rounded={px(8.)}
                        />
                    </div>
                    <div flex flex_col gap={px(7.)}>
                        <div text_xs fontWeight={FontWeight::BOLD} textColor={theme.text_subtle}>
                            {"CONFIRM NEW MASTER PASSWORD"}
                        </div>
                        <Input
                            base={Input::new(&new_master_confirmation)
                                .mask_toggle()
                                .aria_label("Confirm new master password")
                                .disabled(exiting)}
                            h={px(44.)}
                            bg={theme.surface}
                            borderColor={theme.border_strong}
                            rounded={px(8.)}
                        />
                    </div>
                    {error}
                    {restore_button}
                    <div flex flex_col gap={px(8.)}>
                        <div h={px(1.)} w_full bg={theme.border} />
                        {choose_different}
                        <div text_xs text_center textColor={theme.text_ghost}>
                            {"Enter to restore · Esc to go back"}
                        </div>
                    </div>
                    <div flex items_center justify_center gap={px(7.)} textColor={theme.text_subtle}>
                        {gpui_component::Icon::empty().path("icons/shield-check.svg").size(px(13.))}
                        <div text_xs>{"The restored vault remains locked until verified"}</div>
                    </div>
                </div>
            </div>
        };
        page.occlude()
            .with_animation(
                if exiting {
                    "restore-backup-exit"
                } else {
                    "restore-backup-enter"
                },
                Animation::new(RESTORE_TRANSITION_DURATION).with_easing(ease_out_quint()),
                move |page, delta| {
                    let progress = if exiting { 1. - delta } else { delta };
                    page.opacity(progress).left(px((1. - progress) * 24.))
                },
            )
            .into_any_element()
    }
}

pub(crate) fn backup_error_message(operation: &str, error: &BackupError) -> SharedString {
    eprintln!("backup {operation} failed: {error:?}");
    match error {
        BackupError::AuthenticationFailed => {
            "The backup password is incorrect or the backup is damaged.".into()
        }
        BackupError::UnsupportedVersion => "This backup version is not supported.".into(),
        BackupError::InvalidArchive(_) | BackupError::Journal(_) => {
            "The selected file is not a valid Nox backup.".into()
        }
        BackupError::SourceEqualsDestination => {
            "Choose a backup file different from the local vault.".into()
        }
        BackupError::Io(_) if operation == "export" => {
            "Nox could not write the backup to that location.".into()
        }
        BackupError::Io(_) => "Nox could not read or restore the selected backup.".into(),
        _ => "The backup operation failed. Your existing vault was not replaced.".into(),
    }
}
