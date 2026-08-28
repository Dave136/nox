#[path = "backup.rs"]
mod backup;
#[path = "clipboard.rs"]
mod clipboard;
#[path = "conflicts.rs"]
mod conflicts;
#[path = "detail.rs"]
mod detail;
#[path = "item_editor.rs"]
mod item_editor;
#[path = "nav.rs"]
mod nav;
#[path = "settings/mod.rs"]
mod settings;
#[path = "ui/mod.rs"]
pub mod ui;
#[path = "vault_list.rs"]
mod vault_list;

use backup::BackupState;
use clipboard::ClipboardState;
pub use clipboard::DEFAULT_CLIPBOARD_TIMEOUT;
use conflicts::ConflictState;
use item_editor::ItemEditorState;
use nav::ActiveView;
use settings::{Settings, SettingsSection, load_settings, save_settings};
use ui::window::controls::{OpenCommandPalette, WindowCommand, WindowControls};
use vault_list::VaultListState;

use gpui::{
    Animation, AnimationExt, AnyElement, Context, Entity, FontWeight, KeyBinding, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, Render, Rgba, SharedString, Task, Window, div,
    ease_out_quint, prelude::*, px, rgb,
};
use gpui_component::{
    Disableable, Root, Theme, ThemeMode, WindowExt,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    input::{Input, InputState},
};
use gpui_rsx::rsx;
use locker_core::{SecretBytes, Vault, VaultError};
use std::{
    cell::RefCell,
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

use crate::assets::{IconName, icon, logo};

/// Default duration before an inactive unlocked vault is locked.
pub const DEFAULT_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(300);

const UNLOCK_ERROR_MESSAGE: &str = "Incorrect password or corrupted vault.";
const AUTH_HOVER_DURATION: Duration = Duration::from_millis(140);
pub(crate) const CIPHER_BACKGROUND: u32 = 0x1A1D22;
const CIPHER_SURFACE: u32 = 0x1E2126;
pub(crate) const CIPHER_SURFACE_RAISED: u32 = 0x252A33;
pub(crate) const CIPHER_BORDER: u32 = 0x2B3039;
const CIPHER_BORDER_STRONG: u32 = 0x525B69;
const CIPHER_FOREGROUND: u32 = 0xE5E8F0;
const CIPHER_FOREGROUND_SOFT: u32 = 0xD9DEE7;
pub(crate) const CIPHER_FOREGROUND_SECONDARY: u32 = 0xAEB7C5;
pub(crate) const CIPHER_FOREGROUND_MUTED: u32 = 0x8F98A8;
const CIPHER_FOREGROUND_SUBTLE: u32 = 0x7F8998;
const CIPHER_DISABLED: u32 = 0x626B78;
const CIPHER_PRIMARY: u32 = 0xE3E6ED;
const CIPHER_DANGER: u32 = 0xA9787D;

fn auth_hover_color(from: u32, to: u32, amount: f32) -> Rgba {
    let from = rgb(from);
    let to = rgb(to);
    Rgba {
        r: from.r + (to.r - from.r) * amount,
        g: from.g + (to.g - from.g) * amount,
        b: from.b + (to.b - from.b) * amount,
        a: 1.,
    }
}

pub(crate) fn animated_auth_button(
    id: &'static str,
    button: Button,
    hovered: Option<bool>,
    colors: (u32, u32, u32, u32),
    cx: &mut Context<Locker>,
) -> AnyElement {
    let (base, hover, active, foreground) = colors;
    let variant = ButtonCustomVariant::new(cx)
        .foreground(rgb(foreground).into())
        .active(rgb(active).into());
    let button = button.on_hover(cx.listener(move |this, is_hovered, _, cx| {
        this.auth_hovered.insert(id, *is_hovered);
        cx.notify();
    }));
    let Some(hovered) = hovered else {
        return button
            .custom(variant.color(rgb(base).into()).hover(rgb(base).into()))
            .into_any_element();
    };
    button
        .with_animation(
            (id, u32::from(hovered)),
            Animation::new(AUTH_HOVER_DURATION).with_easing(ease_out_quint()),
            move |button, delta| {
                let amount = if hovered { delta } else { 1. - delta };
                let color = auth_hover_color(base, hover, amount);
                button
                    .custom(variant.color(color.into()).hover(color.into()))
                    .opacity(0.96 + amount * 0.04)
            },
        )
        .into_any_element()
}

// ponytail: static display only — no sync status is wired from the `sync`
// crate into the GUI yet, so this always reads "Synced" regardless of the
// vault's real sync/pairing state. Add real wiring if that's ever needed.
fn sync_status_pill() -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(7.))
        .px(px(12.))
        .py(px(9.))
        .rounded(px(8.))
        .border_1()
        .border_color(rgb(CIPHER_BORDER))
        .child(
            gpui_component::Icon::empty()
                .path("icons/cloud-check.svg")
                .size(px(16.))
                .text_color(rgb(CIPHER_FOREGROUND_MUTED)),
        )
        .child(
            div()
                .text_size(px(13.))
                .font_weight(FontWeight(500.))
                .text_color(rgb(CIPHER_FOREGROUND))
                .child("Synced"),
        )
        .into_any_element()
}

impl Locker {
    pub(crate) fn update_settings(
        &mut self,
        mut settings: Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        settings.normalize();
        self.inactivity_timeout = Duration::from_secs(settings.auto_lock_seconds);
        self.clipboard.timeout = Duration::from_secs(settings.clipboard_seconds);
        if let Err(error) = save_settings(&self.vault_path, &settings) {
            eprintln!("settings persistence failed: {error}");
        }
        self.settings = settings;
        self.arm_inactivity_timer(window, cx);
        cx.notify();
    }

    /// The primary "+ Add item" header button, shared by every workspace
    /// header (Home, All items, Logins, Secure notes): matches the Pencil
    /// "Add Item Button" node exactly (`#E3E6ED` fill, dark icon/label) and
    /// reuses the auth screens' animated hover.
    fn render_add_item_button(&mut self, id: &'static str, cx: &mut Context<Self>) -> AnyElement {
        let button = Button::new(id)
            .h(px(38.))
            .px(px(16.))
            .rounded(px(8.))
            .on_click(cx.listener(|this, _, window, cx| this.open_create_editor(window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        gpui_component::Icon::empty()
                            .path("icons/plus.svg")
                            .size(px(16.))
                            .text_color(rgb(CIPHER_BACKGROUND)),
                    )
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight(500.))
                            .text_color(rgb(CIPHER_BACKGROUND))
                            .child(if self.active_view == ActiveView::SecureNotes {
                                "Add note"
                            } else {
                                "Add item"
                            }),
                    ),
            );
        animated_auth_button(
            id,
            button,
            self.auth_hovered.get(id).copied(),
            (CIPHER_PRIMARY, 0xF0F2F6, 0xCDD2DC, CIPHER_BACKGROUND),
            cx,
        )
    }
}

/// Best-effort display label for a recent item's secondary line: the login's
/// site host if it has one, or "Secure note" / a bare "Login" fallback.
fn recent_item_subtitle(payload: &locker_core::ItemPayload) -> String {
    if payload.item_type == locker_core::ItemType::SecureNote {
        return "Secure note".to_owned();
    }
    match payload.uris.first() {
        Some(uri) => uri
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .split('/')
            .next()
            .filter(|host| !host.is_empty())
            .unwrap_or("Login")
            .to_owned(),
        None => "Login".to_owned(),
    }
}

// ponytail: coarse relative-time buckets, no calendar-aware "Yesterday"/weekday
// labels; add if that granularity is ever requested.
fn relative_time(updated_at_ms: u64) -> String {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(updated_at_ms);
    let elapsed_secs = now_ms.saturating_sub(updated_at_ms) / 1000;
    match elapsed_secs {
        0..=59 => "Just now".to_owned(),
        60..=3599 => format!("{}m ago", elapsed_secs / 60),
        3600..=86_399 => format!("{}h ago", elapsed_secs / 3600),
        86_400..=604_799 => format!("{}d ago", elapsed_secs / 86_400),
        _ => format!("{}w ago", elapsed_secs / 604_800),
    }
}

fn home_stat_tile(label: &'static str, value: usize) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .justify_center()
        .gap(px(4.))
        .w(px(112.))
        .h(px(82.))
        .px(px(14.))
        .rounded(px(8.))
        .bg(rgb(0x20242A))
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_color(rgb(CIPHER_FOREGROUND_MUTED))
                .child(label),
        )
        .child(
            div()
                .text_xl()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(CIPHER_FOREGROUND_SECONDARY))
                .child(format!("{value}")),
        )
        .into_any_element()
}

/// A "Quick actions" tile. Matches the Pencil "Quick Actions Row" frame,
/// where every tile (including the not-yet-supported ones) shares the same
/// bright icon/label colors — unsupported tiles are marked with a "Soon"
/// badge rather than dimmed, per the same convention used in the sidebar.
///
/// On hover (of the two clickable tiles) the whole tile animates to the
/// raised surface color, same as the auth screens' submit buttons — see
/// `animated_auth_button`. The icon box swaps to the tile's *resting* color
/// as that happens, so the two backgrounds trade places; that swap is an
/// instant `group_hover`, not part of the animation, matching what was
/// actually asked for (a transition on the tile color, not the icon box).
fn home_quick_action(
    id: &'static str,
    icon_path: &'static str,
    label: &'static str,
    enabled: bool,
    hovered: Option<bool>,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
    cx: &mut Context<Locker>,
) -> AnyElement {
    let mut icon_box = div()
        .size(px(32.))
        .rounded(px(8.))
        .bg(rgb(0x20242A))
        .flex()
        .items_center()
        .justify_center()
        .child(
            gpui_component::Icon::empty()
                .path(icon_path)
                .size(px(16.))
                .text_color(rgb(CIPHER_FOREGROUND_SECONDARY)),
        );
    if enabled {
        icon_box = icon_box.group_hover(id, |style| style.bg(rgb(CIPHER_SURFACE)));
    }
    // Neutralizes Button's own hardcoded `justify_center` on its content
    // wrapper (see `sidebar_link` in nav.rs for the full explanation): a
    // `w_full()` child makes that centering a no-op, so this div's own
    // `justify_between` is what actually places things.
    let mut content = div()
        .flex()
        .items_center()
        .justify_between()
        .w_full()
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(11.))
                .child(icon_box)
                .child(
                    div()
                        .text_sm()
                        .text_color(rgb(CIPHER_FOREGROUND))
                        .child(label),
                ),
        );
    if !enabled {
        content = content.child(
            div()
                .flex_shrink_0()
                .text_size(px(10.))
                .font_weight(FontWeight::from(600.))
                .text_color(rgb(CIPHER_FOREGROUND_MUTED))
                .bg(rgb(CIPHER_SURFACE_RAISED))
                .rounded(px(4.))
                .px(px(6.))
                .py(px(2.))
                .child("Soon"),
        );
    }
    let button = Button::new(id)
        .disabled(!enabled)
        .group(id)
        .flex_1()
        .h(px(62.))
        .px(px(14.))
        .rounded(px(8.))
        .on_click(on_click)
        .child(content);

    if !enabled {
        return button
            .custom(
                ButtonCustomVariant::new(cx)
                    .color(rgb(CIPHER_SURFACE).into())
                    .foreground(rgb(CIPHER_FOREGROUND).into()),
            )
            .into_any_element();
    }
    animated_auth_button(
        id,
        button,
        hovered,
        (
            CIPHER_SURFACE,
            CIPHER_SURFACE_RAISED,
            CIPHER_SURFACE_RAISED,
            CIPHER_FOREGROUND,
        ),
        cx,
    )
}

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
    /// Freshly rendered (title, body) for the open item-editor Sheet, refreshed
    /// every `render_unlocked` pass. See `open_item_editor_sheet` for why this
    /// indirection exists instead of the Sheet reading `Locker` directly.
    item_editor_sheet_cell: Rc<RefCell<Option<(SharedString, AnyElement)>>>,
    active_view: ActiveView,
    /// Whether the selected item's password is shown in plaintext in the detail panel.
    pub(crate) reveal_password: bool,
    pub(crate) clipboard: ClipboardState,
    pub(crate) conflicts: ConflictState,
    pub(crate) conflicts_open: bool,
    pub(crate) backup: BackupState,
    pub(crate) window_controls: Entity<WindowControls>,
    pub(crate) settings: Settings,
    pub(crate) settings_section: SettingsSection,
    pub(crate) settings_open: bool,
    auth_hovered: HashMap<&'static str, bool>,
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
        Theme::change(ThemeMode::Dark, Some(window), cx);
        let create_password = Self::new_input(window, cx, "Create a strong password");
        let create_confirm = Self::new_input(window, cx, "Re-enter your master password");
        let unlock_password = Self::new_input(window, cx, "Password");
        let locker = cx.weak_entity();
        let settings = load_settings(&vault_path);
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
            item_editor_sheet_cell: Rc::new(RefCell::new(None)),
            active_view: ActiveView::AllItems,
            reveal_password: false,
            clipboard: ClipboardState::new(clipboard_timeout),
            conflicts: ConflictState::Closed,
            conflicts_open: false,
            backup: BackupState::new(),
            window_controls,
            settings,
            settings_section: SettingsSection::Appearance,
            settings_open: false,
            auth_hovered: HashMap::new(),
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
        self.create_password = Self::new_input(window, cx, "Create a strong password");
        self.create_confirm = Self::new_input(window, cx, "Re-enter your master password");
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
                            Theme::change(ThemeMode::Dark, Some(window), cx);
                            this.vault_list = Some(VaultListState::from_initial_load(
                                items, deleted, window, cx,
                            ));
                            this.item_editor = None;
                            this.active_view = ActiveView::Home;
                            this.reveal_password = false;
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
                            Theme::change(ThemeMode::Dark, Some(window), cx);
                            this.vault_list = Some(VaultListState::from_initial_load(
                                items, deleted, window, cx,
                            ));
                            this.item_editor = None;
                            this.active_view = ActiveView::Home;
                            this.reveal_password = false;
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
        window.close_sheet(cx);
        self.discard_clipboard_state(cx);
        self.inactivity_epoch += 1;
        self._inactivity_task = Task::ready(());
        self.vault_list = None;
        self.item_editor = None;
        self.active_view = ActiveView::AllItems;
        self.reveal_password = false;
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
        Theme::change(ThemeMode::Dark, Some(window), cx);
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

    fn render_no_vault(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let pending = self.create_state == FormState::Pending;
        let backup_busy = !self.backup.is_idle();
        let error = match &self.create_state {
            FormState::Error(message) => div()
                .text_sm()
                .text_center()
                .text_color(rgb(CIPHER_DANGER))
                .child(message.clone()),
            FormState::Idle | FormState::Pending => div(),
        };
        let backup_status = match &self.backup.operation {
            backup::BackupOperation::Failed(message) => div()
                .text_sm()
                .text_center()
                .text_color(rgb(CIPHER_DANGER))
                .child(message.clone()),
            _ => div(),
        };
        let create_button = Button::new("create-vault-submit")
            .w_full()
            .h(px(44.))
            .rounded(px(7.))
            .disabled(pending || backup_busy)
            .loading(pending)
            .on_click(cx.listener(|this, _, window, cx| this.create_vault(window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .font_weight(FontWeight::BOLD)
                    .child(
                        gpui_component::Icon::empty()
                            .path("icons/shield-plus.svg")
                            .size(px(15.)),
                    )
                    .child(if pending {
                        "Creating…"
                    } else {
                        "Create vault"
                    }),
            );
        let create_button = animated_auth_button(
            "create-vault-submit",
            create_button,
            self.auth_hovered.get("create-vault-submit").copied(),
            (CIPHER_PRIMARY, 0xF0F2F6, 0xCDD2DC, CIPHER_BACKGROUND),
            cx,
        );
        let restore_button = Button::new("create-restore-backup")
            .h(px(32.))
            .disabled(backup_busy)
            .on_click(cx.listener(|this, _, window, cx| this.begin_restore(window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .child(
                        gpui_component::Icon::empty()
                            .path("icons/refresh-cw.svg")
                            .size(px(14.))
                            .text_color(rgb(CIPHER_FOREGROUND_SUBTLE)),
                    )
                    .child("Restore a backup instead"),
            );
        let restore_button = animated_auth_button(
            "create-restore-backup",
            restore_button,
            self.auth_hovered.get("create-restore-backup").copied(),
            (
                CIPHER_BACKGROUND,
                CIPHER_SURFACE_RAISED,
                CIPHER_BORDER,
                CIPHER_FOREGROUND_SECONDARY,
            ),
            cx,
        );
        rsx! {
            <div
                id="no-vault-view"
                size_full
                flex
                items_center
                justify_center
                bg={rgb(CIPHER_BACKGROUND)}
                p={px(36.)}
                onKeyDown={cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    match event.keystroke.key.as_str() {
                        "enter" => this.create_vault(window, cx),
                        "escape" => window.remove_window(),
                        _ => {}
                    }
                })}
            >
                <div
                    id="no-vault-card"
                    flex
                    flex_col
                    gap={px(15.)}
                    w={px(416.)}
                >
                    <div flex flex_col items_center gap={px(8.)}>
                        {logo(52., rgb(CIPHER_FOREGROUND).into())}
                        <div flex flex_col items_center gap={px(4.)}>
                            <div text_lg fontWeight={FontWeight::SEMIBOLD} textColor={rgb(CIPHER_FOREGROUND)}>
                                {"Create your vault"}
                            </div>
                            <div text_xs text_center textColor={rgb(CIPHER_FOREGROUND_MUTED)}>
                                {"Choose a master password to secure your data"}
                            </div>
                        </div>
                    </div>
                    <div
                        id="create-recovery-warning"
                        flex
                        items_center
                        gap={px(10.)}
                        h={px(48.)}
                        px={px(12.)}
                        rounded={px(8.)}
                        bg={rgb(CIPHER_SURFACE)}
                        border_1
                        borderColor={rgb(CIPHER_BORDER)}
                    >
                        <div size={px(28.)} flex items_center justify_center rounded_full bg={rgb(CIPHER_SURFACE_RAISED)}>
                            <div text_sm fontWeight={FontWeight::BOLD} textColor={rgb(CIPHER_FOREGROUND)}>{"!"}</div>
                        </div>
                        <div flex flex_col flex_1 gap={px(2.)}>
                            <div text_sm fontWeight={FontWeight::SEMIBOLD} textColor={rgb(CIPHER_FOREGROUND_SOFT)}>
                                {"No password recovery"}
                            </div>
                            <div text_xs textColor={rgb(CIPHER_FOREGROUND_SUBTLE)}>
                                {"Store your master password somewhere safe"}
                            </div>
                        </div>
                        {gpui_component::Icon::empty()
                            .path("icons/shield-alert.svg")
                            .size(px(14.))
                            .text_color(rgb(CIPHER_FOREGROUND_SUBTLE))}
                    </div>
                    <div id="create-vault-password" flex flex_col gap={px(7.)}>
                        <div text_xs fontWeight={FontWeight::BOLD} textColor={rgb(CIPHER_FOREGROUND_SUBTLE)}>
                            {"MASTER PASSWORD"}
                        </div>
                        <Input
                            base={Input::new(&self.create_password)
                                .mask_toggle()
                                .aria_label("Master password")}
                            h={px(44.)}
                            bg={rgb(CIPHER_SURFACE)}
                            borderColor={rgb(CIPHER_BORDER_STRONG)}
                            rounded={px(8.)}
                        />
                    </div>
                    <div id="create-vault-confirm" flex flex_col gap={px(7.)}>
                        <div text_xs fontWeight={FontWeight::BOLD} textColor={rgb(CIPHER_FOREGROUND_SUBTLE)}>
                            {"CONFIRM MASTER PASSWORD"}
                        </div>
                        <Input
                            base={Input::new(&self.create_confirm)
                                .mask_toggle()
                                .aria_label("Confirm master password")}
                            h={px(44.)}
                            bg={rgb(CIPHER_SURFACE)}
                            borderColor={rgb(CIPHER_BORDER_STRONG)}
                            rounded={px(8.)}
                        />
                    </div>
                    {error}
                    {create_button}
                    <div flex flex_col gap={px(8.)}>
                        <div h={px(1.)} w_full bg={rgb(CIPHER_BORDER)} />
                        {restore_button}
                        {backup_status}
                        <div text_xs text_center textColor={rgb(CIPHER_DISABLED)}>
                            {"Enter to create · Esc to close"}
                        </div>
                    </div>
                    <div flex items_center justify_center gap={px(7.)} textColor={rgb(CIPHER_FOREGROUND_SUBTLE)}>
                        {gpui_component::Icon::empty().path("icons/shield-check.svg").size(px(13.))}
                        <div text_xs>{"Encrypted locally · You hold the keys"}</div>
                    </div>
                </div>
            </div>
        }
    }

    fn render_locked(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let pending = self.unlock_state == FormState::Pending;
        let backup_busy = !self.backup.is_idle();
        let vault_name = self
            .vault_path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("Local vault")
            .to_owned();
        let error = match &self.unlock_state {
            FormState::Error(message) => div()
                .text_sm()
                .text_color(rgb(CIPHER_DANGER))
                .child(message.clone()),
            FormState::Idle | FormState::Pending => div(),
        };
        let backup_status = match &self.backup.operation {
            backup::BackupOperation::Failed(message) => div()
                .text_sm()
                .text_center()
                .text_color(rgb(CIPHER_DANGER))
                .child(message.clone()),
            backup::BackupOperation::Succeeded(message) => div()
                .text_sm()
                .text_center()
                .text_color(rgb(CIPHER_FOREGROUND_SUBTLE))
                .child(message.clone()),
            backup::BackupOperation::AwaitingRestoreConfirmation { archive_path } => div()
                .text_sm()
                .text_center()
                .text_color(rgb(CIPHER_FOREGROUND_SUBTLE))
                .child(format!("Restore: {}", archive_path.display())),
            _ => div(),
        };
        let unlock_button = Button::new("unlock-submit")
            .w_full()
            .h(px(44.))
            .rounded(px(7.))
            .disabled(pending || backup_busy)
            .loading(pending)
            .on_click(cx.listener(|this, _, window, cx| this.unlock_vault(window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .font_weight(FontWeight::BOLD)
                    .child(icon(
                        IconName::KeySquare,
                        Some(15.),
                        Some(rgb(CIPHER_BACKGROUND).into()),
                    ))
                    .child(if pending {
                        "Unlocking…"
                    } else {
                        "Unlock vault"
                    }),
            );
        let unlock_button = animated_auth_button(
            "unlock-submit",
            unlock_button,
            self.auth_hovered.get("unlock-submit").copied(),
            (CIPHER_PRIMARY, 0xF0F2F6, 0xCDD2DC, CIPHER_BACKGROUND),
            cx,
        );
        let restore_button = Button::new("restore-backup")
            .h(px(32.))
            .disabled(backup_busy)
            .on_click(cx.listener(|this, _, window, cx| this.begin_restore(window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .child(
                        gpui_component::Icon::empty()
                            .path("icons/refresh-cw.svg")
                            .size(px(14.))
                            .text_color(rgb(CIPHER_FOREGROUND_SUBTLE)),
                    )
                    .child("Restore a backup instead"),
            );
        let restore_button = animated_auth_button(
            "restore-backup",
            restore_button,
            self.auth_hovered.get("restore-backup").copied(),
            (
                CIPHER_BACKGROUND,
                CIPHER_SURFACE_RAISED,
                CIPHER_BORDER,
                CIPHER_FOREGROUND_SECONDARY,
            ),
            cx,
        );
        rsx! {
            <div
                id="locked-view"
                size_full
                flex
                items_center
                justify_center
                bg={rgb(CIPHER_BACKGROUND)}
                p={px(36.)}
                onKeyDown={cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key == "enter" {
                        this.unlock_vault(window, cx);
                    }
                })}
            >
                <div
                    id="locked-card"
                    flex
                    flex_col
                    gap={px(18.)}
                    w={px(416.)}
                >
                    <div flex flex_col items_center gap={px(8.)}>
                        {logo(52., rgb(CIPHER_FOREGROUND).into())}
                        <div flex flex_col items_center gap={px(4.)}>
                            <div text_lg fontWeight={FontWeight::SEMIBOLD} textColor={rgb(CIPHER_FOREGROUND)}>
                                {"Unlock your vault"}
                            </div>
                            <div text_xs text_center textColor={rgb(CIPHER_FOREGROUND_MUTED)}>
                                {"Enter your master password to continue"}
                            </div>
                        </div>
                    </div>
                    <div
                        id="locked-vault-summary"
                        flex
                        items_center
                        gap={px(10.)}
                        h={px(48.)}
                        px={px(12.)}
                        rounded={px(8.)}
                        bg={rgb(CIPHER_SURFACE)}
                        border_1
                        borderColor={rgb(CIPHER_BORDER)}
                    >
                        <div size={px(28.)} flex items_center justify_center rounded_full bg={rgb(CIPHER_SURFACE_RAISED)}>
                            {logo(14., rgb(CIPHER_FOREGROUND).into())}
                        </div>
                        <div flex flex_col flex_1 gap={px(2.)}>
                            <div text_sm fontWeight={FontWeight::SEMIBOLD} textColor={rgb(CIPHER_FOREGROUND_SOFT)}>
                                {"Local vault"}
                            </div>
                            <div text_xs textColor={rgb(CIPHER_FOREGROUND_SUBTLE)}>
                                {format!("{vault_name} · Local")}
                            </div>
                        </div>
                        {icon(IconName::KeySquare, Some(14.), Some(rgb(CIPHER_FOREGROUND_SUBTLE).into()))}
                    </div>
                    <div id="unlock-password" flex flex_col gap={px(7.)}>
                        <div text_xs fontWeight={FontWeight::BOLD} textColor={rgb(CIPHER_FOREGROUND_SUBTLE)}>
                            {"MASTER PASSWORD"}
                        </div>
                        <Input
                            base={Input::new(&self.unlock_password)
                                .mask_toggle()
                                .aria_label("Master password")}
                            h={px(44.)}
                            bg={rgb(CIPHER_SURFACE)}
                            borderColor={rgb(CIPHER_BORDER_STRONG)}
                            rounded={px(8.)}
                        />
                    </div>
                    {error}
                    {unlock_button}
                    <div flex flex_col gap={px(8.)}>
                        <div h={px(1.)} w_full bg={rgb(CIPHER_BORDER)} />
                        {restore_button}
                        {backup_status}
                        <div text_xs text_center textColor={rgb(CIPHER_DISABLED)}>
                            {"Enter to unlock"}
                        </div>
                    </div>
                    <div flex items_center justify_center gap={px(7.)} textColor={rgb(CIPHER_FOREGROUND_SUBTLE)}>
                        {gpui_component::Icon::empty()
                            .path("icons/shield-check.svg")
                            .size(px(13.))}
                        <div text_xs>{"Encrypted locally · Works offline"}</div>
                    </div>
                </div>
            </div>
        }
    }

    fn render_unlocked(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.active_view == ActiveView::Home {
            let nav = self.render_sidebar_nav(cx);
            let home = self.render_home(window, cx);
            return rsx! { <div id="home-shell" size_full flex bg={rgb(CIPHER_BACKGROUND)}>{nav}{home}</div> };
        }
        if self.uses_secure_note_workspace() {
            let nav = self.render_sidebar_nav(cx);
            let workspace = self.render_secure_note_workspace(window, cx);
            return rsx! {
                <div id="secure-note-workspace-shell" size_full flex bg={rgb(0xF7F8FA)}>
                    {nav}
                    {workspace}
                </div>
            };
        }
        if self.uses_login_workspace() {
            let nav = self.render_sidebar_nav(cx);
            let workspace = self.render_login_workspace(window, cx);
            return rsx! {
                <div id="login-workspace-shell" size_full flex bg={rgb(CIPHER_BACKGROUND)}>
                    {nav}
                    {workspace}
                </div>
            };
        }
        if self.item_editor.is_some() {
            let locker = cx.entity();
            let content = self.render_item_editor(locker, window, cx);
            *self.item_editor_sheet_cell.borrow_mut() = Some(content);
        }
        let nav = self.render_sidebar_nav(cx);
        let add = self.render_add_item_button("header-add-item", cx);
        let list_toolbar = self.render_vault_list_toolbar(cx);
        let list = self.render_vault_list(window, cx);
        let detail = self.render_item_detail(window, cx);
        let conflict_panel = if self.conflicts_open {
            div()
                .p(px(20.))
                .pb(px(0.))
                .child(self.render_conflicts(window, cx))
                .into_any_element()
        } else {
            div().into_any_element()
        };
        let (total, logins, notes) = self.vault_list.as_ref().map_or((0, 0, 0), |list| {
            let logins = list
                .items
                .iter()
                .filter(|(_, item)| item.item_type == locker_core::ItemType::Login)
                .count();
            let notes = list
                .items
                .iter()
                .filter(|(_, item)| item.item_type == locker_core::ItemType::SecureNote)
                .count();
            (list.items.len(), logins, notes)
        });
        let (page_title, item_count, item_noun) = match self.active_view {
            ActiveView::Home => ("Home", total, "items"),
            ActiveView::AllItems => ("All items", total, "items"),
            ActiveView::Logins => ("Logins", logins, "logins"),
            ActiveView::SecureNotes => ("Secure Notes", notes, "encrypted notes"),
        };
        rsx! {
            <div
                id="unlocked-view"
                size_full
                flex
                bg={rgb(CIPHER_BACKGROUND)}
                onMouseMove={cx.listener(|this, _: &MouseMoveEvent, _, cx| {
                    this.note_activity(cx);
                })}
                onMouseDown={(MouseButton::Left, cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    this.note_activity(cx);
                }))}
                onKeyDown={cx.listener(|this, _: &KeyDownEvent, _, cx| {
                    this.note_activity(cx);
                })}
            >
                {nav}
                <div flex flex_col flex_1 min_w={px(0.)} h_full>
                    <div
                        id="content-toolbar"
                        flex
                        items_center
                        justify_between
                        w_full
                        h={px(88.)}
                        px={px(32.)}
                        flex_shrink_0
                        border_b_1
                        borderColor={rgb(CIPHER_BORDER)}
                    >
                        <div flex flex_col gap={px(3.)}>
                            <div text_xl fontWeight={FontWeight::SEMIBOLD} textColor={rgb(CIPHER_FOREGROUND)}>{page_title}</div>
                            <div text_xs textColor={rgb(CIPHER_FOREGROUND_MUTED)}>
                                {if self.active_view == ActiveView::SecureNotes {
                                    format!("{item_count} {item_noun}")
                                } else {
                                    format!("{item_count} {item_noun} in your vault")
                                }}
                            </div>
                        </div>
                        <div flex items_center gap={px(10.)}>
                            <Button
                                base={Button::new("lock-vault")
                                    .ghost()
                                    .icon(gpui_component::Icon::empty().path("icons/lock-keyhole-open.svg").text_color(rgb(CIPHER_FOREGROUND_MUTED)))
                                    .tooltip("Lock vault")
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
                                    )}
                            />
                            {sync_status_pill()}
                            {add}
                        </div>
                    </div>
                    {conflict_panel}
                    <div flex flex_col flex_1 min_h={px(0.)} p={px(28.)} pt={px(20.)} gap={px(16.)} bg={rgb(CIPHER_BACKGROUND)}>
                        {list_toolbar}
                        <div flex flex_1 min_h={px(0.)} gap={px(16.)}>
                            {list}
                            {detail}
                        </div>
                    </div>
                </div>
            </div>
        }
    }

    fn render_home(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let (total, logins, notes, recent) =
            self.vault_list
                .as_ref()
                .map_or((0, 0, 0, Vec::new()), |list| {
                    let logins = list
                        .items
                        .iter()
                        .filter(|(_, item)| item.item_type == locker_core::ItemType::Login)
                        .count();
                    let notes = list
                        .items
                        .iter()
                        .filter(|(_, item)| item.item_type == locker_core::ItemType::SecureNote)
                        .count();
                    let recent = list
                        .items
                        .iter()
                        .rev()
                        .take(5)
                        .map(|(id, item)| (*id, item.clone()))
                        .collect::<Vec<_>>();
                    (list.items.len(), logins, notes, recent)
                });
        let locker = cx.entity();
        let add = self.render_add_item_button("home-add-item", cx);
        let view_all = Button::new("home-view-all")
            .ghost()
            .h(px(26.))
            .label("View all ›")
            .text_color(rgb(CIPHER_FOREGROUND_SECONDARY))
            .on_click({
                let locker = locker.clone();
                move |_, _window, cx| {
                    locker.update(cx, |locker, cx| {
                        locker.set_active_view(ActiveView::AllItems, cx)
                    });
                }
            });
        let recent_rows: Vec<AnyElement> = recent
            .into_iter()
            .map(|(item_id, item)| {
                let is_login = item.item_type == locker_core::ItemType::Login;
                let icon_path = if is_login {
                    "icons/key-square.svg"
                } else {
                    "icons/file-lock.svg"
                };
                let title = if item.title.is_empty() {
                    "Untitled".to_owned()
                } else {
                    item.title.clone()
                };
                let subtitle = recent_item_subtitle(&item);
                let time = relative_time(item.updated_at);
                let row_locker = locker.clone();
                Button::new(SharedString::from(format!("home-recent-{item_id}")))
                    .ghost()
                    .w_full()
                    .h(px(64.))
                    .justify_start()
                    .px(px(18.))
                    .border_b_1()
                    .border_color(rgb(CIPHER_BORDER))
                    .on_click(move |_, window, cx| {
                        row_locker.update(cx, |locker, cx| {
                            locker.open_editor_for_item(item_id, false, window, cx)
                        });
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .w_full()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(14.))
                                    .child(
                                        div()
                                            .size(px(34.))
                                            .rounded(px(8.))
                                            .bg(rgb(CIPHER_SURFACE_RAISED))
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(
                                                gpui_component::Icon::empty()
                                                    .path(icon_path)
                                                    .size(px(15.))
                                                    .text_color(rgb(CIPHER_FOREGROUND_SECONDARY)),
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
                                                    .text_color(rgb(CIPHER_FOREGROUND))
                                                    .child(title),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(rgb(CIPHER_FOREGROUND_MUTED))
                                                    .child(subtitle),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(16.))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(rgb(CIPHER_FOREGROUND_MUTED))
                                            .child(time),
                                    )
                                    .child(
                                        gpui_component::Icon::empty()
                                            .path("icons/ellipsis-vertical.svg")
                                            .size(px(14.))
                                            .text_color(rgb(CIPHER_FOREGROUND_MUTED)),
                                    ),
                            ),
                    )
                    .into_any_element()
            })
            .collect();
        let recent_body = if recent_rows.is_empty() {
            div()
                .py(px(40.))
                .text_sm()
                .text_center()
                .text_color(rgb(CIPHER_FOREGROUND_MUTED))
                .child("No items yet")
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_col()
                .children(recent_rows)
                .into_any_element()
        };
        let new_login = home_quick_action(
            "home-new-login",
            "icons/key-square.svg",
            "New login",
            true,
            self.auth_hovered.get("home-new-login").copied(),
            {
                let locker = locker.clone();
                move |_, window, cx| {
                    locker.update(cx, |l, cx| {
                        l.active_view = ActiveView::Logins;
                        l.open_create_editor(window, cx);
                    });
                }
            },
            cx,
        );
        let new_note = home_quick_action(
            "home-secure-note",
            "icons/file-lock.svg",
            "Secure note",
            true,
            self.auth_hovered.get("home-secure-note").copied(),
            {
                let locker = locker.clone();
                move |_, window, cx| {
                    locker.update(cx, |l, cx| {
                        l.active_view = ActiveView::SecureNotes;
                        l.open_create_editor(window, cx);
                    });
                }
            },
            cx,
        );
        let new_card = home_quick_action(
            "home-payment-card",
            "icons/credit-card.svg",
            "Payment card",
            false,
            None,
            |_, _, _| {},
            cx,
        );
        let new_identity = home_quick_action(
            "home-identity",
            "icons/user.svg",
            "Identity",
            false,
            None,
            |_, _, _| {},
            cx,
        );
        rsx! {
            <div id="home-workspace" flex flex_col flex_1 min_w={px(0.)} h_full bg={rgb(CIPHER_BACKGROUND)}>
                <div id="home-header" flex items_center justify_between h={px(88.)} px={px(32.)} flex_shrink_0 border_b_1 borderColor={rgb(CIPHER_BORDER)}>
                    <div flex flex_col gap={px(3.)}>
                        <div text_xl fontWeight={FontWeight::SEMIBOLD} textColor={rgb(CIPHER_FOREGROUND)}>{"Home"}</div>
                        <div text_xs textColor={rgb(CIPHER_FOREGROUND_MUTED)}>{"Your vault at a glance"}</div>
                    </div>
                    <div flex items_center gap={px(10.)}>
                        {sync_status_pill()}
                        {add}
                    </div>
                </div>
                <div id="home-dashboard" flex flex_col gap={px(20.)} p={px(32.)} overflow_y_scroll>
                    <div id="home-hero" flex items_start justify_between p={px(24.)} rounded={px(10.)} bg={rgb(CIPHER_SURFACE_RAISED)}>
                        <div flex flex_col gap={px(8.)} w={px(360.)}>
                            <div text_xs fontWeight={FontWeight::BOLD} textColor={rgb(CIPHER_FOREGROUND_MUTED)}>{"WELCOME BACK"}</div>
                            <div text_lg fontWeight={FontWeight::SEMIBOLD} textColor={rgb(CIPHER_FOREGROUND)}>{"Your vault is secure"}</div>
                            <div text_sm textColor={rgb(CIPHER_FOREGROUND_MUTED)}>
                                {format!(
                                    "Stored locally and encrypted. {total} item{} in your vault.",
                                    if total == 1 { "" } else { "s" },
                                )}
                            </div>
                        </div>
                        <div flex gap={px(12.)}>
                            {home_stat_tile("VAULT ITEMS", total)}
                            {home_stat_tile("LOGINS", logins)}
                            {home_stat_tile("SECURE NOTES", notes)}
                        </div>
                    </div>
                    <div id="home-quick-actions" flex flex_col gap={px(12.)}>
                        <div flex items_center justify_between>
                            <div text_sm fontWeight={FontWeight::SEMIBOLD} textColor={rgb(CIPHER_FOREGROUND)}>{"Quick actions"}</div>
                            <div text_xs textColor={rgb(CIPHER_FOREGROUND_MUTED)}>{"Ctrl+P to search or create"}</div>
                        </div>
                        <div flex gap={px(12.)}>
                            {new_login}
                            {new_note}
                            {new_card}
                            {new_identity}
                        </div>
                    </div>
                    <div flex gap={px(20.)} items_start>
                        <div id="home-recent-items" flex flex_col flex_1 min_w={px(0.)} rounded={px(10.)} bg={rgb(CIPHER_SURFACE)}>
                            <div flex items_center justify_between h={px(58.)} px={px(18.)} border_b_1 borderColor={rgb(CIPHER_BORDER)}>
                                <div flex flex_col gap={px(2.)}>
                                    <div text_sm fontWeight={FontWeight::SEMIBOLD} textColor={rgb(CIPHER_FOREGROUND)}>{"Recent items"}</div>
                                    <div text_xs textColor={rgb(CIPHER_FOREGROUND_MUTED)}>{"Newest first"}</div>
                                </div>
                                {view_all}
                            </div>
                            {recent_body}
                        </div>
                        <div flex flex_col gap={px(16.)} w={px(330.)} flex_shrink_0>
                            <div flex flex_col gap={px(10.)} p={px(18.)} rounded={px(10.)} bg={rgb(CIPHER_SURFACE)}>
                                <div flex items_center gap={px(8.)}>
                                    {gpui_component::Icon::empty().path("icons/shield-check.svg").size(px(15.)).text_color(rgb(CIPHER_FOREGROUND_SECONDARY))}
                                    <div text_sm fontWeight={FontWeight::SEMIBOLD} textColor={rgb(CIPHER_FOREGROUND)}>{"Security health"}</div>
                                </div>
                                <div text_xs textColor={rgb(CIPHER_FOREGROUND_MUTED)}>{"Password health scoring isn't available yet."}</div>
                            </div>
                            <div flex flex_col gap={px(10.)} p={px(18.)} rounded={px(10.)} bg={rgb(CIPHER_SURFACE)}>
                                <div flex items_center gap={px(8.)}>
                                    {gpui_component::Icon::empty().path("icons/star.svg").size(px(15.)).text_color(rgb(CIPHER_FOREGROUND_SECONDARY))}
                                    <div text_sm fontWeight={FontWeight::SEMIBOLD} textColor={rgb(CIPHER_FOREGROUND)}>{"Favorites"}</div>
                                </div>
                                <div text_xs textColor={rgb(CIPHER_FOREGROUND_MUTED)}>{"Favoriting items isn't available yet."}</div>
                            </div>
                        </div>
                    </div>
                </div>
            </div>
        }.into_any_element()
    }
}

impl Render for Locker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let authenticated = matches!(&self.state, AppState::Unlocked(_));
        self.window_controls.update(cx, |controls, cx| {
            controls.set_authenticated(authenticated, cx)
        });
        let body = if matches!(
            self.backup.dialog,
            Some(backup::BackupDialogState::Restore { .. })
        ) {
            self.render_restore_backup(window, cx)
        } else {
            match &self.state {
                AppState::NoVault => self.render_no_vault(window, cx).into_any_element(),
                AppState::Locked => self.render_locked(window, cx).into_any_element(),
                AppState::Unlocked(_) => self.render_unlocked(window, cx).into_any_element(),
            }
        };
        let dialog_layer = Root::render_dialog_layer(window, cx);
        let sheet_layer = Root::render_sheet_layer(window, cx);
        let settings_modal = self.settings_open.then(|| {
            settings::render_settings_modal(
                cx.entity(),
                self.settings.clone(),
                self.settings_section,
                self.conflicts.count(),
            )
        });
        rsx! {
            <div
                size_full
                relative
                flex
                flex_col
                bg={rgb(CIPHER_BACKGROUND)}
                onAction={cx.listener(|this, _: &OpenCommandPalette, window, cx| {
                    // open_palette itself no-ops while unauthenticated.
                    this.window_controls
                        .update(cx, |controls, cx| controls.open_palette(window, cx));
                })}
            >
                {self.window_controls.clone()}
                <div flex_1 bg={rgb(CIPHER_BACKGROUND)}>{body}</div>
                {for modal in settings_modal {
                    {modal}
                }}
                {for dialog in dialog_layer {
                    {dialog}
                }}
                {for sheet in sheet_layer {
                    {sheet}
                }}
            </div>
        }
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
    use gpui_component::{ActiveTheme, Root, Theme, ThemeMode, WindowExt};
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
        assert_eq!(cx.update(|_, app| app.theme().mode), ThemeMode::Dark);
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
        assert_eq!(cx.update(|_, app| app.theme().mode), ThemeMode::Dark);
        cleanup(&path);
    }

    #[gpui::test]
    fn locked_view_uses_dark_component_theme(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("locked-theme");
        Vault::create(b"correct", &path).unwrap().lock();
        let (_view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);

        assert_eq!(cx.update(|_, app| app.theme().mode), ThemeMode::Dark);
        cleanup(&path);
    }

    #[gpui::test]
    fn create_view_uses_dark_component_theme(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("create-theme");
        let (_view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);

        assert_eq!(cx.update(|_, app| app.theme().mode), ThemeMode::Dark);
        cleanup(&path);
    }

    #[gpui::test]
    fn settings_dialog_renders_a_full_window_backdrop(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("settings-backdrop");
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_settings_dialog(window, locker_cx);
        });
        cx.executor().advance_clock(Duration::from_millis(180));
        cx.run_until_parked();

        let backdrop = cx.debug_bounds("settings-backdrop").unwrap();
        let dialog = cx.debug_bounds("settings-dialog-shell").unwrap();
        assert_eq!(dialog.size.width, px(1040.));
        assert_eq!(dialog.size.height, px(800.));
        assert!(backdrop.size.width >= dialog.size.width);
        assert!(backdrop.size.height >= dialog.size.height);
        assert!(!cx.update(|window, app| window.has_active_dialog(app)));
        assert!(view.read_with(cx, |locker, _| locker.settings_open));

        cx.simulate_click(dialog.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        assert!(view.read_with(cx, |locker, _| locker.settings_open));

        cx.simulate_click(
            gpui::point(backdrop.origin.x + px(8.), backdrop.origin.y + px(8.)),
            gpui::Modifiers::none(),
        );
        cx.run_until_parked();
        assert!(cx.debug_bounds("settings-backdrop").is_none());
        assert!(!view.read_with(cx, |locker, _| locker.settings_open));
        cleanup(&path);
    }

    #[test]
    fn auth_hover_color_interpolates_between_state_colors() {
        assert_eq!(auth_hover_color(0x000000, 0xFFFFFF, 0.), rgb(0x000000));
        assert_eq!(auth_hover_color(0x000000, 0xFFFFFF, 1.), rgb(0xFFFFFF));
    }

    #[gpui::test]
    fn locking_restores_dark_component_theme(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("lock-theme");
        let vault = Vault::create(b"correct", &path).unwrap();
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        cx.update(|_, app| Theme::change(ThemeMode::Light, None, app));
        view.update_in(cx, |locker, window, locker_cx| {
            locker.state = AppState::Unlocked(vault);
            locker.lock_vault(window, locker_cx);
        });

        assert_eq!(cx.update(|_, app| app.theme().mode), ThemeMode::Dark);
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
    fn create_page_escape_closes_the_window(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("create-escape");
        let (_view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);

        cx.simulate_keystrokes("escape");
        assert!(cx.windows().is_empty());
        cleanup(&path);
    }

    #[gpui::test]
    fn home_view_can_be_selected_for_unlocked_vault(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "home-view", &[]);
        view.update_in(cx, |locker, _window, locker_cx| {
            locker.set_active_view(ActiveView::Home, locker_cx);
        });
        assert_eq!(
            view.read_with(cx, |locker, _| locker.active_view),
            ActiveView::Home
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
    fn secure_note_search_matches_note_contents(cx: &mut TestAppContext) {
        init(cx);
        let payloads = [ItemPayload {
            schema_version: ITEM_SCHEMA_VERSION,
            item_type: ItemType::SecureNote,
            title: "Recovery codes".into(),
            username: String::new(),
            password: String::new(),
            uris: Vec::new(),
            notes: "Use the vault phrase amber-galaxy to recover access.".into(),
            created_at: 1,
            updated_at: 1,
        }];
        let (view, cx, path, _) = unlocked_view(cx, "secure-note-content-search", &payloads);
        view.update_in(cx, |locker, _window, locker_cx| {
            locker.active_view = ActiveView::SecureNotes;
            locker.recompute_vault_list_filter(locker_cx);
        });
        view.update_in(cx, |locker, _window, locker_cx| {
            locker
                .vault_list
                .as_mut()
                .unwrap()
                .recompute_filter("amber-galaxy".into());
            locker_cx.notify();
        });
        assert_eq!(
            view.read_with(cx, |locker, _| locker
                .vault_list
                .as_ref()
                .unwrap()
                .filtered
                .len()),
            1
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
    fn secure_note_creation_uses_the_dedicated_workspace(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "secure-note-workspace", &[]);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.set_active_view(ActiveView::SecureNotes, locker_cx);
            locker.open_create_editor(window, locker_cx);
        });
        assert!(view.read_with(cx, |locker, _| locker.uses_secure_note_workspace()));
        assert!(!cx.update(|window, app| window.has_active_sheet(app)));
        cleanup(&path);
    }

    #[gpui::test]
    fn secure_note_requires_a_title_before_saving(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "secure-note-title-required", &[]);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.set_active_view(ActiveView::SecureNotes, locker_cx);
            locker.open_create_editor(window, locker_cx);
            locker.save_item(window, locker_cx);
        });
        assert_eq!(
            view.read_with(cx, |locker, _| locker
                .item_editor
                .as_ref()
                .and_then(|editor| editor.save_error.as_ref())
                .map(ToString::to_string)),
            Some("Enter a title.".to_owned())
        );
        cleanup(&path);
    }

    #[gpui::test]
    fn secure_note_requires_content_before_saving(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "secure-note-content-required", &[]);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.set_active_view(ActiveView::SecureNotes, locker_cx);
            locker.open_create_editor(window, locker_cx);
            let title = locker.item_editor.as_ref().unwrap().title_input.clone();
            title.update(locker_cx, |input, input_cx| {
                input.set_value("Recovery codes", window, input_cx)
            });
            locker.save_item(window, locker_cx);
        });
        assert_eq!(
            view.read_with(cx, |locker, _| locker
                .item_editor
                .as_ref()
                .and_then(|editor| editor.save_error.as_ref())
                .map(ToString::to_string)),
            Some("Enter note content.".to_owned())
        );
        cleanup(&path);
    }

    /// Mirrors `secure_note_creation_uses_the_dedicated_workspace`: creating a
    /// login from the Logins view uses the full-page "Create login" workspace
    /// instead of the generic Sheet, and actually renders it (real password
    /// strength/reuse checks included) without panicking.
    #[gpui::test]
    fn login_creation_uses_the_dedicated_workspace(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) =
            unlocked_view(cx, "login-workspace", &[login_payload("Existing", "alex")]);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.set_active_view(ActiveView::Logins, locker_cx);
            locker.open_create_editor(window, locker_cx);
        });
        assert!(view.read_with(cx, |locker, _| locker.uses_login_workspace()));
        assert!(!cx.update(|window, app| window.has_active_sheet(app)));

        // Type the vault's one existing password into the draft and confirm
        // the real reuse check flags it, then cancel via Escape.
        view.update_in(cx, |locker, window, locker_cx| {
            let editor = locker.item_editor.as_ref().unwrap();
            let password_input = editor.password_input.clone();
            password_input.update(locker_cx, |state, input_cx| {
                state.set_value("secret".to_owned(), window, input_cx);
            });
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |locker, _| locker.item_editor.is_some()));

        view.update_in(cx, |locker, window, locker_cx| {
            locker.cancel_item_editor(window, locker_cx);
        });
        assert!(view.read_with(cx, |locker, _| locker.item_editor.is_none()));
        cleanup(&path);
    }

    /// Renders the real Logins split view (not just the pure health-check
    /// functions) with two logins sharing the same short "secret" password —
    /// weak *and* reused — end to end: view switch, health-filter pills,
    /// selecting a row, and the health-filter toggle all run without panicking.
    #[gpui::test]
    fn logins_view_renders_with_real_weak_and_reused_passwords(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, ids) = unlocked_view(
            cx,
            "logins-health",
            &[
                login_payload("GitHub", "alex"),
                login_payload("AWS", "alex"),
            ],
        );
        view.update_in(cx, |locker, _window, locker_cx| {
            locker.set_active_view(ActiveView::Logins, locker_cx);
        });
        cx.run_until_parked();
        let (weak, reused) = view.read_with(cx, |locker, _| {
            let list = locker.vault_list.as_ref().unwrap();
            (list.weak_login_count(), list.reused_login_count())
        });
        assert_eq!((weak, reused), (2, 2));

        view.update_in(cx, |locker, window, locker_cx| {
            locker.select_item(ids[0], locker_cx);
            if let Some(list) = locker.vault_list.as_mut() {
                list.set_login_health_filter(Some(vault_list::LoginHealth::Reused));
            }
            let _ = window;
        });
        cx.run_until_parked();
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

    /// The detail pane's "Duplicate" footer button: a real second item, not a
    /// UI-only copy — persisted via the same `Vault::create_item` path a
    /// normal save uses, titled "<original> (copy)", and selected afterward.
    #[gpui::test]
    fn duplicate_item_persists_a_titled_copy_and_selects_it(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, ids) =
            unlocked_view(cx, "duplicate-item", &[login_payload("Original", "alice")]);
        let original_id = ids[0];
        view.update_in(cx, |locker, _window, locker_cx| {
            locker.duplicate_item(original_id, locker_cx);
        });
        let (item_count, titles, selected_is_new, persisted_count) =
            view.read_with(cx, |locker, _| {
                let list = locker.vault_list.as_ref().unwrap();
                let titles: Vec<_> = list.items.iter().map(|(_, p)| p.title.clone()).collect();
                let persisted_count = match &locker.state {
                    AppState::Unlocked(vault) => vault.list_items().unwrap().len(),
                    _ => 0,
                };
                (
                    list.items.len(),
                    titles,
                    list.selected.is_some_and(|id| id != original_id),
                    persisted_count,
                )
            });
        assert_eq!(item_count, 2);
        assert_eq!(persisted_count, 2);
        assert!(titles.contains(&"Original".to_owned()));
        assert!(titles.contains(&"Original (copy)".to_owned()));
        assert!(selected_is_new);
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
    fn restore_confirmation_uses_the_inline_page_instead_of_a_dialog(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("inline-restore");
        let archive = path.with_extension("lockbak");
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);

        view.update_in(cx, |locker, window, locker_cx| {
            locker.request_restore_confirmation(archive.clone(), window, locker_cx);
            assert!(!window.has_active_dialog(locker_cx));
        });
        assert!(view.read_with(cx, |locker, _| {
            matches!(
                &locker.backup.dialog,
                Some(backup::BackupDialogState::Restore { archive_path, .. }) if archive_path == &archive
            )
        }));
        cleanup(&path);
    }

    #[gpui::test]
    fn restore_form_reports_missing_passwords_inline(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("restore-validation");
        let archive = path.with_extension("lockbak");
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);

        view.update_in(cx, |locker, window, locker_cx| {
            locker.request_restore_confirmation(archive, window, locker_cx);
            assert!(!locker.confirm_restore(window, locker_cx));
            assert_eq!(
                locker.backup.restore_error.as_deref(),
                Some("Enter all three passwords.")
            );
        });
        cleanup(&path);
    }

    #[gpui::test]
    fn restore_back_waits_for_the_exit_transition(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("restore-exit");
        let archive = path.with_extension("lockbak");
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.request_restore_confirmation(archive, window, locker_cx);
            locker.leave_restore(window, locker_cx);
            assert!(locker.backup.restore_exiting);
            assert!(locker.backup.dialog.is_some());
        });

        cx.executor().advance_clock(Duration::from_millis(180));
        cx.run_until_parked();
        assert!(view.read_with(cx, |locker, _| {
            !locker.backup.restore_exiting
                && locker.backup.dialog.is_none()
                && matches!(locker.backup.operation, backup::BackupOperation::Idle)
        }));
        cleanup(&path);
    }

    #[gpui::test]
    fn reduced_motion_restore_back_is_immediate(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("restore-reduced-motion");
        let archive = path.with_extension("lockbak");
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        cx.update(|_, app| app.set_reduce_motion(true));

        view.update_in(cx, |locker, window, locker_cx| {
            locker.request_restore_confirmation(archive, window, locker_cx);
            locker.leave_restore(window, locker_cx);
        });
        assert!(view.read_with(cx, |locker, _| {
            locker.backup.dialog.is_none()
                && matches!(locker.backup.operation, backup::BackupOperation::Idle)
        }));
        cleanup(&path);
    }

    #[gpui::test]
    fn restore_page_enter_submits_the_form(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("restore-enter");
        let archive = path.with_extension("lockbak");
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.request_restore_confirmation(archive, window, locker_cx);
        });

        cx.simulate_keystrokes("enter");
        assert_eq!(
            view.read_with(cx, |locker, _| locker.backup.restore_error.clone()),
            Some("Enter all three passwords.".into())
        );
        cleanup(&path);
    }

    #[gpui::test]
    fn restore_page_escape_starts_the_back_transition(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("restore-escape");
        let archive = path.with_extension("lockbak");
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.request_restore_confirmation(archive, window, locker_cx);
        });

        cx.simulate_keystrokes("escape");
        assert!(view.read_with(cx, |locker, _| locker.backup.restore_exiting));
        cleanup(&path);
    }

    #[gpui::test]
    fn restore_submit_is_ignored_during_the_exit_transition(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("restore-submit-during-exit");
        let archive = path.with_extension("lockbak");
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.request_restore_confirmation(archive, window, locker_cx);
            locker.leave_restore(window, locker_cx);
            assert!(!locker.confirm_restore(window, locker_cx));
            assert_eq!(locker.backup.restore_error, None);
        });
        cleanup(&path);
    }

    #[gpui::test]
    fn choosing_a_different_restore_reopens_the_picker_after_exit(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("restore-repick");
        let archive = path.with_extension("lockbak");
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.request_restore_confirmation(archive, window, locker_cx);
            locker.choose_different_restore(window, locker_cx);
        });

        cx.executor().advance_clock(Duration::from_millis(180));
        cx.run_until_parked();
        assert!(view.read_with(cx, |locker, _| matches!(
            locker.backup.operation,
            backup::BackupOperation::ChoosingRestorePath
        )));
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
            Theme::change(ThemeMode::Dark, Some(window), locker_cx);
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
        assert_eq!(cx.update(|_, app| app.theme().mode), ThemeMode::Dark);
        assert!(cx.update(|window, app| { input.read(app).focus_handle(app).is_focused(window) }));
        cleanup(&path);
    }
}
