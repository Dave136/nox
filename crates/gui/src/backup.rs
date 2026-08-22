use super::{AppState, Locker};
use gpui::{
    AnyElement, Context, Entity, PathPromptOptions, SharedString, Task, Window, div, prelude::*,
};
use gpui_component::{
    ActiveTheme, Disableable, Sizable, WindowExt,
    button::Button,
    input::{Input, InputState},
};
use locker_core::{
    BackupError, MAX_BACKUP_PASSWORD_BYTES, RestoreResult, SecretBytes, restore_from_path,
};
use std::path::{Path, PathBuf};

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
}

impl BackupState {
    pub(crate) fn new() -> Self {
        Self {
            operation: BackupOperation::Idle,
            dialog: None,
            epoch: 0,
            task: Task::ready(()),
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
    cx: &mut Context<Locker>,
    placeholder: &'static str,
) -> Entity<InputState> {
    cx.new(|cx| {
        InputState::new(window, cx)
            .masked(true)
            .placeholder(placeholder)
    })
}

impl Locker {
    pub(crate) fn begin_export(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.state, AppState::Unlocked(_)) || !self.backup.is_idle() {
            return;
        }
        self.backup.epoch = self.backup.epoch.wrapping_add(1);
        let epoch = self.backup.epoch;
        self.backup.operation = BackupOperation::ChoosingExportPath;
        self.backup.dialog = None;
        let directory = self
            .vault_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let receiver = cx.prompt_for_new_path(&directory, Some("locker-backup.lockbak"));
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
                .child("Locker cannot recover this backup password. Without it, the backup cannot be restored.")
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
            AppState::Unlocked(vault) => {
                vault.prepare_backup_export(password.as_bytes(), &destination)
            }
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
            prompt: Some("Choose Locker backup".into()),
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
        let backup_password = masked_input(window, cx, "Backup password");
        let new_master_password = masked_input(window, cx, "New master password");
        let new_master_confirmation = masked_input(window, cx, "Confirm new master password");
        self.backup.operation = BackupOperation::AwaitingRestoreConfirmation {
            archive_path: archive_path.clone(),
        };
        self.backup.dialog = Some(BackupDialogState::Restore {
            backup_password: backup_password.clone(),
            new_master_password: new_master_password.clone(),
            new_master_confirmation: new_master_confirmation.clone(),
            archive_path: archive_path.clone(),
        });
        let destination = self.vault_path.clone();
        let locker = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |dialog, _window, _cx| {
            let ok_locker = locker.clone();
            let cancel_locker = locker.clone();
            dialog
                .title("Restore backup")
                .child(format!("Archive: {}", archive_path.display()))
                .child(format!("This will replace: {}", destination.display()))
                .child(
                    "The restored vault stays locked. The new master password cannot be recovered.",
                )
                .child(Input::new(&backup_password).mask_toggle())
                .child(Input::new(&new_master_password).mask_toggle())
                .child(Input::new(&new_master_confirmation).mask_toggle())
                .confirm()
                .on_ok(move |_, window, app| {
                    let mut accepted = false;
                    let _ = ok_locker.update(app, |locker, cx| {
                        accepted = locker.confirm_restore(window, cx);
                    });
                    accepted
                })
                .on_cancel(move |_, _, app| {
                    let _ = cancel_locker.update(app, |locker, cx| locker.cancel_backup_dialog(cx));
                    true
                })
        });
    }

    pub(crate) fn confirm_restore(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
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
        if backup.is_empty()
            || master.is_empty()
            || master.as_bytes() != confirmation.as_bytes()
            || backup.len() > MAX_BACKUP_PASSWORD_BYTES
            || master.len() > MAX_BACKUP_PASSWORD_BYTES
        {
            self.backup.dialog = Some(BackupDialogState::Restore {
                backup_password: backup_password_input,
                new_master_password: master_password_input,
                new_master_confirmation: master_confirmation_input,
                archive_path,
            });
            return false;
        }
        let was_no_vault = matches!(self.state, AppState::NoVault);
        window.close_all_dialogs(cx);
        self.discard_clipboard_state(cx);
        self.vault_list = None;
        self.item_editor = None;
        self.conflicts = super::conflicts::ConflictState::Closed;
        self.conflicts_open = false;
        if let AppState::Unlocked(vault) = std::mem::replace(&mut self.state, AppState::Locked) {
            vault.lock();
        }
        self.reset_unlock_input(window, cx);
        self.backup.epoch = self.backup.epoch.wrapping_add(1);
        let epoch = self.backup.epoch;
        self.backup.operation = BackupOperation::Restoring;
        let backup_secret = backup;
        let master_secret = master;
        let destination = self.vault_path.clone();
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
                if was_no_vault && !self.vault_path.exists() {
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
        self.backup.task = Task::ready(());
        cx.notify();
    }

    pub(crate) fn render_backup_actions(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let busy = !self.backup.is_idle();
        let danger = cx.theme().danger;
        let muted_foreground = cx.theme().muted_foreground;
        let restore = Button::new("restore-backup")
            .small()
            .label("Restore backup")
            .disabled(busy)
            .on_click(cx.listener(|this, _, window, cx| this.begin_restore(window, cx)));
        let mut row = div().flex().items_center().gap_2().child(restore);
        if matches!(self.state, AppState::Unlocked(_)) {
            row = row.child(
                Button::new("export-backup")
                    .small()
                    .label("Export backup")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, window, cx| this.begin_export(window, cx))),
            );
        }
        if let BackupOperation::Failed(message) = &self.backup.operation {
            row = row.child(div().text_sm().text_color(danger).child(message.clone()));
        } else if let BackupOperation::Succeeded(message) = &self.backup.operation {
            row = row.child(
                div()
                    .text_sm()
                    .text_color(muted_foreground)
                    .child(message.clone()),
            );
        } else if let BackupOperation::AwaitingRestoreConfirmation { archive_path } =
            &self.backup.operation
        {
            row = row.child(
                div()
                    .text_sm()
                    .text_color(muted_foreground)
                    .child(format!("Restore: {}", archive_path.display())),
            );
        }
        row.into_any_element()
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
            "The selected file is not a valid Locker backup.".into()
        }
        BackupError::SourceEqualsDestination => {
            "Choose a backup file different from the local vault.".into()
        }
        BackupError::Io(_) if operation == "export" => {
            "Locker could not write the backup to that location.".into()
        }
        BackupError::Io(_) => "Locker could not read or restore the selected backup.".into(),
        _ => "The backup operation failed. Your existing vault was not replaced.".into(),
    }
}
