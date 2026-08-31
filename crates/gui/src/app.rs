use crate::backup::{self, BackupState};
use crate::clipboard::ClipboardState;
pub use crate::clipboard::DEFAULT_CLIPBOARD_TIMEOUT;
use crate::conflicts::ConflictState;
use crate::item_editor::{self, ItemEditorState};
use crate::nav::ActiveView;
use crate::settings::{self, Settings, SettingsSection, load_settings, save_settings};
use crate::theme::Theme;
use crate::ui::window::controls::{OpenCommandPalette, WindowCommand, WindowControls};
use crate::vault_list::VaultListState;
use crate::vaults::{
    VaultEntry, VaultRegistry, adopt_legacy_vault, load_registry, save_registry, slug_for,
};

use gpui::{
    Animation, AnimationExt, AnyElement, BoxShadow, Context, Entity, FocusHandle, FontWeight, Hsla,
    KeyBinding, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, Render, Rgba,
    SharedString, Subscription, Task, Window, div, ease_out_quint, point, prelude::*, px, relative,
    rgb, rgba,
};
use gpui_component::{
    Disableable, FocusTrapElement as _, IndexPath, Root, Sizable, ThemeMode, WindowExt,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    input::{Input, InputState},
    popover::Popover,
    select::{SelectDelegate, SelectEvent, SelectItem, SelectState},
};
use gpui_rsx::rsx;
use nox_core::{SecretBytes, Vault, VaultError};
use std::{
    cell::RefCell,
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::assets::logo;

/// Default duration before an inactive unlocked vault is locked.
pub const DEFAULT_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(300);

const UNLOCK_ERROR_MESSAGE: &str = "Incorrect password or corrupted vault.";
const AUTH_HOVER_DURATION: Duration = Duration::from_millis(140);
/// Interpolate two theme roles for the hover transition.
///
/// The mix happens in RGB, not in the `Hsla` the roles are stored as: hue is
/// circular, so lerping it takes near-greys on a detour through unrelated
/// hues. Converting to `Rgba` first keeps the original channel-wise math.
fn auth_hover_color(from: Hsla, to: Hsla, amount: f32) -> Hsla {
    let from: Rgba = from.into();
    let to: Rgba = to.into();
    Rgba {
        r: from.r + (to.r - from.r) * amount,
        g: from.g + (to.g - from.g) * amount,
        b: from.b + (to.b - from.b) * amount,
        a: 1.,
    }
    .into()
}

pub(crate) fn relative_opened_label(last_opened_ms: Option<u64>) -> SharedString {
    let Some(last_opened_ms) = last_opened_ms else {
        return "never opened".into();
    };
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let elapsed = now_ms.saturating_sub(last_opened_ms);
    if elapsed < 60_000 {
        "just now".into()
    } else if elapsed < 3_600_000 {
        format!("{}m ago", elapsed / 60_000).into()
    } else if elapsed < 86_400_000 {
        format!("{}h ago", elapsed / 3_600_000).into()
    } else {
        format!("{}d ago", elapsed / 86_400_000).into()
    }
}

fn vault_initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|part| part.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase()
}

fn vault_dropdown_trailing(theme: Theme, missing: bool, checked: bool) -> (&'static str, Hsla) {
    if missing {
        ("icons/circle-x.svg", theme.text_ghost)
    } else if checked {
        ("icons/check.svg", theme.text_soft)
    } else {
        ("icons/chevron-right.svg", theme.icon_muted)
    }
}

/// Shared avatar + name + status row content for the vault Select — used
/// both as the trigger's `display_title` (no trailing icon; the Select adds
/// its own caret) and as each dropdown option's `render` (with one).
fn vault_row_content(theme: Theme, vault: &VaultEntry, trailing: Option<AnyElement>) -> AnyElement {
    let missing = !vault.path.is_file();
    let secondary = if missing {
        "File not found".to_owned()
    } else {
        format!("Local · {}", relative_opened_label(vault.last_opened_ms))
    };
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .w_full()
        .child(
            div()
                .size(px(28.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(theme.raised)
                .child(
                    div()
                        .text_size(px(9.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(if missing {
                            theme.text_ghost
                        } else {
                            theme.text
                        })
                        .child(vault_initials(&vault.name)),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .gap(px(2.))
                .child(
                    div()
                        .text_size(px(11.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(if missing {
                            theme.text_ghost
                        } else {
                            theme.text_soft
                        })
                        .child(vault.name.clone()),
                )
                .child(
                    div()
                        .text_size(px(9.))
                        .text_color(if missing {
                            theme.text_ghost
                        } else {
                            theme.text_subtle
                        })
                        .child(secondary),
                ),
        )
        .when_some(trailing, |row, trailing| row.child(trailing))
        .into_any_element()
}

impl SelectItem for VaultEntry {
    type Value = String;

    fn title(&self) -> SharedString {
        self.name.clone().into()
    }

    fn display_title(&self) -> Option<AnyElement> {
        // `SelectItem` hands no `cx` to its renderers, so the palette can't be
        // read from the global here. Nox ships one theme, so naming it
        // directly is exact rather than a guess.
        Some(vault_row_content(Theme::cipher_midnight(), self, None))
    }

    fn value(&self) -> &Self::Value {
        &self.id
    }

    fn disabled(&self) -> bool {
        !self.path.is_file()
    }
}

/// Delegate retained as the picker state/event source; the locked view renders
/// its exact Pencil chrome through an unstyled `Popover`.
#[derive(Clone)]
pub(crate) struct VaultDelegate(pub(crate) Vec<VaultEntry>);

impl SelectDelegate for VaultDelegate {
    type Item = VaultEntry;

    fn items_count(&self, _section: usize) -> usize {
        self.0.len()
    }

    fn item(&self, ix: IndexPath) -> Option<&Self::Item> {
        self.0.as_slice().get(ix.row)
    }

    fn position<V>(&self, value: &V) -> Option<IndexPath>
    where
        Self::Item: SelectItem<Value = V>,
        V: PartialEq,
    {
        self.0
            .iter()
            .position(|vault| vault.value() == value)
            .map(|ix| IndexPath::default().row(ix))
    }
}

pub(crate) fn animated_auth_button(
    id: &'static str,
    button: Button,
    hovered: Option<bool>,
    colors: (Hsla, Hsla, Hsla, Hsla),
    cx: &mut Context<Nox>,
) -> AnyElement {
    let (base, hover, active, foreground) = colors;
    let variant = ButtonCustomVariant::new(cx)
        .foreground(foreground)
        .active(active);
    let button = button.on_hover(cx.listener(move |this, is_hovered, _, cx| {
        this.auth_hovered.insert(id, *is_hovered);
        cx.notify();
    }));
    let Some(hovered) = hovered else {
        return button
            .custom(variant.color(base).hover(base))
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
                    .custom(variant.color(color).hover(color))
                    .opacity(0.96 + amount * 0.04)
            },
        )
        .into_any_element()
}

// ponytail: static display only — no sync status is wired from the `sync`
// crate into the GUI yet, so this always reads "Synced" regardless of the
// vault's real sync/pairing state. Add real wiring if that's ever needed.
fn sync_status_pill(theme: Theme) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(px(7.))
        .px(px(12.))
        .py(px(9.))
        .rounded(px(8.))
        .border_1()
        .border_color(theme.border)
        .child(
            gpui_component::Icon::empty()
                .path("icons/cloud-check.svg")
                .size(px(16.))
                .text_color(theme.text_muted),
        )
        .child(
            div()
                .text_size(px(13.))
                .font_weight(FontWeight(500.))
                .text_color(theme.text)
                .child("Synced"),
        )
        .into_any_element()
}

impl Nox {
    pub(crate) fn update_settings(
        &mut self,
        mut settings: Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        settings.normalize();
        self.inactivity_timeout = Duration::from_secs(settings.auto_lock_seconds);
        self.clipboard.timeout = Duration::from_secs(settings.clipboard_seconds);
        if let Err(error) = save_settings(&self.data_dir, &settings) {
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
        let theme = Theme::current(cx);
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
                            .text_color(theme.canvas),
                    )
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight(500.))
                            .text_color(theme.canvas)
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
            (
                theme.inverse_bright,
                theme.inverse,
                theme.inverse_press,
                theme.canvas,
            ),
            cx,
        )
    }
}

/// Best-effort display label for a recent item's secondary line: the login's
/// site host if it has one, or "Secure note" / a bare "Login" fallback.
fn recent_item_subtitle(payload: &nox_core::ItemPayload) -> String {
    if payload.item_type == nox_core::ItemType::SecureNote {
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
pub(crate) fn relative_time(updated_at_ms: u64) -> String {
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

fn home_stat_tile(theme: Theme, label: &'static str, value: usize) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .justify_center()
        .gap(px(4.))
        .w(px(112.))
        .h(px(82.))
        .px(px(14.))
        .rounded(px(8.))
        .bg(theme.field)
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_color(theme.text_muted)
                .child(label),
        )
        .child(
            div()
                .text_xl()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text_secondary)
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
    cx: &mut Context<Nox>,
) -> AnyElement {
    let theme = Theme::current(cx);
    let mut icon_box = div()
        .size(px(32.))
        .rounded(px(8.))
        .bg(theme.field)
        .flex()
        .items_center()
        .justify_center()
        .child(
            gpui_component::Icon::empty()
                .path(icon_path)
                .size(px(16.))
                .text_color(theme.text_secondary),
        );
    if enabled {
        icon_box = icon_box.group_hover(id, |style| style.bg(theme.surface));
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
                .child(div().text_sm().text_color(theme.text).child(label)),
        );
    if !enabled {
        content = content.child(
            div()
                .flex_shrink_0()
                .text_size(px(10.))
                .font_weight(FontWeight::from(600.))
                .text_color(theme.text_muted)
                .bg(theme.raised)
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
                    .color(theme.surface)
                    .foreground(theme.text),
            )
            .into_any_element();
    }
    animated_auth_button(
        id,
        button,
        hovered,
        (theme.surface, theme.raised, theme.raised, theme.text),
        cx,
    )
}

/// The secure-note workspace is the app's one light surface — a "paper" ground
/// for long-form note editing. Cipher Midnight has no role for it, and
/// inventing one would imply a light palette that does not exist.
const SECURE_NOTE_PAPER: Rgba = Rgba {
    r: 0.969,
    g: 0.973,
    b: 0.980,
    a: 1.,
};

/// The top-level vault lifecycle state.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum AppState {
    NoVault,
    RegistryError,
    Locked,
    Unlocked(Vault),
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum FormState {
    Idle,
    Pending,
    Error(SharedString),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RemoveVaultDialogState {
    index: usize,
    name: SharedString,
    file_exists: bool,
    delete_files: bool,
}

#[derive(Clone)]
struct RenameVaultDialogState {
    index: usize,
    original: SharedString,
    input: Entity<InputState>,
    /// Set when the user confirms an empty name: the dialog stays open and
    /// says why, rather than closing on a name that was never applied.
    error: bool,
}

/// Root Nox view for vault creation, unlock, and lock lifecycle actions.
pub struct Nox {
    pub(crate) state: AppState,
    pub(crate) data_dir: PathBuf,
    pub(crate) vaults: VaultRegistry,
    pub(crate) active_vault: Option<VaultEntry>,
    /// Vault picker shown inline on the Locked screen's account row.
    pub(crate) vault_select: Entity<SelectState<VaultDelegate>>,
    _vault_select_subscription: Subscription,

    create_name: Entity<InputState>,
    pub(crate) create_password: Entity<InputState>,
    create_confirm: Entity<InputState>,
    create_state: FormState,
    _create_task: Task<()>,

    pub(crate) unlock_password: Entity<InputState>,
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
    /// indirection exists instead of the Sheet reading `Nox` directly.
    pub(crate) item_editor_sheet_cell: Rc<RefCell<Option<(SharedString, AnyElement)>>>,
    pub(crate) active_view: ActiveView,
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
    remove_vault_dialog: Option<RemoveVaultDialogState>,
    rename_vault_dialog: Option<RenameVaultDialogState>,
    rename_vault_dialog_focus: FocusHandle,
    rename_vault_cancel_focus: FocusHandle,
    rename_vault_confirm_focus: FocusHandle,
    rename_vault_prior_focus: Option<FocusHandle>,
    remove_vault_dialog_focus: FocusHandle,
    remove_vault_option_focus: FocusHandle,
    remove_vault_cancel_focus: FocusHandle,
    remove_vault_confirm_focus: FocusHandle,
    remove_vault_prior_focus: Option<FocusHandle>,
    pub(crate) auth_hovered: HashMap<&'static str, bool>,
}

impl Nox {
    /// Construct a Nox view from the app data directory and vault registry.
    pub fn new(
        data_dir: PathBuf,
        inactivity_timeout: Duration,
        clipboard_timeout: Duration,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let loaded = load_registry(&data_dir);
        let mut vaults = loaded.registry;
        if !loaded.unreadable
            && adopt_legacy_vault(&data_dir, &mut vaults)
            && let Err(error) = save_registry(&data_dir, &vaults)
        {
            eprintln!("vault registry persistence failed: {error}");
        }
        let state = if loaded.unreadable {
            AppState::RegistryError
        } else {
            match vaults.vaults.len() {
                0 => AppState::NoVault,
                _ => AppState::Locked,
            }
        };
        // With one vault it's the only choice; with several, the inline picker
        // on the Locked screen preselects the last-opened one, falling back to
        // the first registered entry.
        let active_vault = match vaults.vaults.as_slice() {
            [] => None,
            [vault] => Some(vault.clone()),
            _ => vaults
                .last_opened
                .as_ref()
                .and_then(|id| vaults.vaults.iter().find(|vault| &vault.id == id).cloned())
                .or_else(|| vaults.vaults.first().cloned()),
        };
        let active_index = active_vault
            .as_ref()
            .and_then(|active| vaults.vaults.iter().position(|vault| vault.id == active.id));
        let vault_select = cx.new(|cx| {
            SelectState::new(
                VaultDelegate(vaults.vaults.clone()),
                active_index.map(IndexPath::new),
                window,
                cx,
            )
        });
        let _vault_select_subscription = cx.subscribe_in(
            &vault_select,
            window,
            |this: &mut Self, _select, event: &SelectEvent<VaultDelegate>, window, cx| {
                let SelectEvent::Confirm(value) = event;
                this.active_vault = value.as_ref().and_then(|id| {
                    this.vaults
                        .vaults
                        .iter()
                        .find(|vault| &vault.id == id)
                        .cloned()
                });
                this.unlock_state = FormState::Idle;
                this.reset_unlock_input(window, cx);
                Self::focus_input(&this.unlock_password, window, cx);
                cx.notify();
            },
        );
        crate::theme::apply(ThemeMode::Dark, Some(window), cx);
        let create_name = Self::new_input(window, cx, "Personal vault", false);
        let create_password = Self::new_input(window, cx, "Create a strong password", true);
        let create_confirm = Self::new_input(window, cx, "Re-enter your master password", true);
        let unlock_password = Self::new_input(window, cx, "Password", true);
        let locker = cx.weak_entity();
        let settings = load_settings(&data_dir);
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
            data_dir,
            vaults,
            active_vault,
            vault_select,
            _vault_select_subscription,
            create_name,
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
            remove_vault_dialog: None,
            rename_vault_dialog: None,
            rename_vault_dialog_focus: cx.focus_handle(),
            rename_vault_cancel_focus: cx.focus_handle().tab_stop(true),
            rename_vault_confirm_focus: cx.focus_handle().tab_stop(true),
            rename_vault_prior_focus: None,
            remove_vault_dialog_focus: cx.focus_handle(),
            remove_vault_option_focus: cx.focus_handle().tab_stop(true),
            remove_vault_cancel_focus: cx.focus_handle().tab_stop(true),
            remove_vault_confirm_focus: cx.focus_handle().tab_stop(true),
            remove_vault_prior_focus: None,
            auth_hovered: HashMap::new(),
        };

        match locker.state {
            AppState::NoVault => Self::focus_input(&locker.create_name, window, cx),
            AppState::Locked => Self::focus_input(&locker.unlock_password, window, cx),
            AppState::RegistryError | AppState::Unlocked(_) => {}
        }
        locker
    }

    pub(crate) fn vault_path(&self) -> Option<&Path> {
        self.active_vault.as_ref().map(|vault| vault.path.as_path())
    }

    fn new_input(
        window: &mut Window,
        cx: &mut Context<Self>,
        placeholder: &'static str,
        masked: bool,
    ) -> Entity<InputState> {
        cx.new(|cx| {
            InputState::new(window, cx)
                .masked(masked)
                .placeholder(placeholder)
        })
    }

    pub(crate) fn focus_input(
        input: &Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        input.update(cx, |input, cx| input.focus(window, cx));
    }

    pub(crate) fn reset_create_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.create_name = Self::new_input(window, cx, "Personal vault", false);
        self.create_password = Self::new_input(window, cx, "Create a strong password", true);
        self.create_confirm = Self::new_input(window, cx, "Re-enter your master password", true);
    }

    pub(crate) fn reset_unlock_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.unlock_password = Self::new_input(window, cx, "Password", true);
    }

    fn new_vault_path(&self, name: &str) -> PathBuf {
        let slug = slug_for(name, &self.vaults.vaults);
        self.data_dir.join("vaults").join(slug).join("vault.db")
    }

    fn record_vault_opened(&mut self, entry: &VaultEntry) {
        let now = item_editor::now_millis();
        if let Some(existing) = self
            .vaults
            .vaults
            .iter_mut()
            .find(|vault| vault.id == entry.id)
        {
            existing.name = entry.name.clone();
            existing.path = entry.path.clone();
            existing.last_opened_ms = Some(now);
        } else {
            let mut entry = entry.clone();
            entry.last_opened_ms = Some(now);
            self.vaults.vaults.push(entry);
        }
        self.vaults.last_opened = Some(entry.id.clone());
    }

    /// Returns to the Locked screen's inline vault picker — used by the File
    /// menu's "Open Vault" command. Locks first if a vault is unlocked; the
    /// vault Select keeps its own last selection, so no index bookkeeping
    /// is needed here.
    fn return_to_unlock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.lock_vault(window, cx);
        self.state = AppState::Locked;
        self.reset_unlock_input(window, cx);
        cx.notify();
    }

    /// Drop the vault at `index` from the list, optionally deleting its files.
    ///
    /// `delete_files` is the confirmation dialog's opt-in modifier, not a
    /// default: removing is metadata-only unless the user explicitly asked for
    /// the file to go too.
    pub(crate) fn remove_vault(
        &mut self,
        index: usize,
        delete_files: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.vaults.vaults.get(index).cloned() else {
            return;
        };

        // Lock first when this vault is the one currently open: deleting its
        // file out from under an unlocked `Vault` would skip the existing
        // teardown that zeroizes key material, clears the clipboard secret and
        // disarms the timers.
        if delete_files
            && matches!(&self.state, AppState::Unlocked(_))
            && self.active_vault.as_ref().is_some_and(|v| v.id == entry.id)
        {
            self.lock_vault(window, cx);
        }

        // Before `remove_entry`, while the path is still reachable.
        if delete_files {
            crate::vaults::delete_vault_files(&entry.path);
        }
        crate::vaults::remove_entry(&mut self.vaults, index);
        if let Err(error) = save_registry(&self.data_dir, &self.vaults) {
            eprintln!("vault registry persistence failed: {error}");
        }

        if self.active_vault.as_ref().is_some_and(|v| v.id == entry.id) {
            self.active_vault = self
                .vaults
                .vaults
                .iter()
                .find(|vault| vault.path.is_file())
                .or_else(|| self.vaults.vaults.first())
                .cloned();
        }
        self.sync_vault_select(window, cx);

        if self.vaults.vaults.is_empty() {
            self.state = AppState::NoVault;
            self.active_vault = None;
            self.create_state = FormState::Idle;
            self.reset_create_inputs(window, cx);
            Self::focus_input(&self.create_name, window, cx);
        } else if !matches!(&self.state, AppState::Unlocked(_)) {
            self.state = AppState::Locked;
            self.unlock_state = FormState::Idle;
            self.reset_unlock_input(window, cx);
        }
        cx.notify();
    }

    /// Rename the vault at `index`. Returns whether the name was accepted.
    ///
    /// Metadata only: the entry's `path` is untouched, so a vault renamed to
    /// "Work" keeps living under its original slug directory. Moving an
    /// encrypted database to match a label is the riskier operation and buys
    /// nothing the user can see — the row shows the folder as metadata, not
    /// as identity.
    pub(crate) fn rename_vault(
        &mut self,
        index: usize,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let name = name.trim();
        // Refused rather than defaulted: silently naming a vault something the
        // user never typed is worse than making them try again.
        if name.is_empty() {
            return false;
        }
        let Some(entry) = self.vaults.vaults.get_mut(index) else {
            return false;
        };
        if entry.name != name {
            entry.name = name.to_owned();
            let renamed = entry.clone();
            if self
                .active_vault
                .as_ref()
                .is_some_and(|active| active.id == renamed.id)
            {
                self.active_vault = Some(renamed);
            }
            if let Err(error) = save_registry(&self.data_dir, &self.vaults) {
                eprintln!("vault registry persistence failed: {error}");
            }
            // Without this the Locked screen's trigger keeps the old label.
            self.sync_vault_select(window, cx);
            cx.notify();
        }
        true
    }

    /// Open the in-app rename dialog for the vault at `index`.
    pub(crate) fn begin_rename_vault(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.vaults.vaults.get(index) else {
            return;
        };
        let original: SharedString = entry.name.clone().into();
        let input = Self::new_input(window, cx, "Vault name", false);
        input.update(cx, |input, cx| {
            input.set_value(original.to_string(), window, cx);
        });
        self.rename_vault_prior_focus = window.focused(cx);
        self.rename_vault_dialog = Some(RenameVaultDialogState {
            index,
            original,
            input: input.clone(),
            error: false,
        });
        // The name is what the user came to change, so the field takes focus.
        Self::focus_input(&input, window, cx);
        cx.notify();
    }

    fn cancel_rename_vault(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.rename_vault_dialog = None;
        if let Some(focus) = self.rename_vault_prior_focus.take() {
            focus.focus(window, cx);
        }
        cx.notify();
    }

    fn confirm_rename_vault(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.rename_vault_dialog.clone() else {
            return;
        };
        let name = dialog.input.read(cx).value().to_string();
        if self.rename_vault(dialog.index, name, window, cx) {
            self.rename_vault_dialog = None;
            self.rename_vault_prior_focus = None;
            cx.notify();
        } else if let Some(state) = self.rename_vault_dialog.as_mut() {
            state.error = true;
            Self::focus_input(&dialog.input, window, cx);
            cx.notify();
        }
    }

    /// Open the in-app confirmation for removing the vault at `index`.
    pub(crate) fn begin_remove_vault(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.vaults.vaults.get(index) else {
            return;
        };
        self.remove_vault_prior_focus = window.focused(cx);
        self.remove_vault_dialog = Some(RemoveVaultDialogState {
            index,
            name: entry.name.clone().into(),
            file_exists: entry.path.is_file(),
            // Removing only unregisters the vault until the user deliberately
            // opts into the destructive path.
            delete_files: false,
        });
        self.remove_vault_cancel_focus.focus(window, cx);
        cx.notify();
    }

    fn toggle_remove_vault_files(&mut self, cx: &mut Context<Self>) {
        if let Some(dialog) = &mut self.remove_vault_dialog
            && dialog.file_exists
        {
            dialog.delete_files = !dialog.delete_files;
            cx.notify();
        }
    }

    fn cancel_remove_vault(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.remove_vault_dialog = None;
        if let Some(focus) = self.remove_vault_prior_focus.take() {
            focus.focus(window, cx);
        }
        cx.notify();
    }

    fn confirm_remove_vault(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.remove_vault_dialog.take() else {
            return;
        };
        self.remove_vault_prior_focus = None;
        self.remove_vault(dialog.index, dialog.delete_files, window, cx);
    }

    fn render_rename_vault_dialog(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.rename_vault_dialog.clone()?;
        let cancel_focus = self.rename_vault_cancel_focus.clone();
        let confirm_focus = self.rename_vault_confirm_focus.clone();
        let dialog_focus = self.rename_vault_dialog_focus.clone();
        let cancel_for_a11y = cx.entity();
        let confirm_for_a11y = cx.entity();

        let cancel = div()
            .id("rename-vault-cancel")
            .debug_selector(|| "rename-vault-cancel".to_owned())
            .w(px(69.))
            .h(px(34.))
            .px(px(16.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(7.))
            .border_1()
            .border_color(rgb(0x3A4450))
            .bg(rgb(0x222731))
            .font_family("Inter")
            .text_size(px(11.))
            .font_weight(FontWeight(550.))
            .text_color(rgb(0xDDE3E8))
            .cursor_pointer()
            .track_focus(&cancel_focus)
            .role(gpui::Role::Button)
            .aria_label("Cancel")
            .focus_visible(|style| style.border_color(rgb(0x77B8DF)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    window.prevent_default();
                    this.cancel_rename_vault(window, cx);
                }
            }))
            .on_click(cx.listener(|this, _, window, cx| this.cancel_rename_vault(window, cx)))
            .on_a11y_action(gpui::AccessibleAction::Click, move |_, window, app| {
                cancel_for_a11y.update(app, |this, cx| this.cancel_rename_vault(window, cx));
            })
            .child("Cancel");

        let confirm = div()
            .id("rename-vault-confirm")
            .debug_selector(|| "rename-vault-confirm".to_owned())
            .w(px(64.))
            .h(px(34.))
            .px(px(16.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(7.))
            .border_1()
            .border_color(rgb(0xE3E6ED))
            .bg(rgb(0xE3E6ED))
            .font_family("Inter")
            .text_size(px(11.))
            .font_weight(FontWeight(650.))
            .text_color(rgb(0x1A1D22))
            .cursor_pointer()
            .track_focus(&confirm_focus)
            .role(gpui::Role::Button)
            .aria_label("Save")
            .focus_visible(|style| style.border_color(rgb(0x77B8DF)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    window.prevent_default();
                    this.confirm_rename_vault(window, cx);
                }
            }))
            .on_click(cx.listener(|this, _, window, cx| this.confirm_rename_vault(window, cx)))
            .on_a11y_action(gpui::AccessibleAction::Click, move |_, window, app| {
                confirm_for_a11y.update(app, |this, cx| this.confirm_rename_vault(window, cx));
            })
            .child("Save");

        let error = dialog.error.then(|| {
            div()
                .debug_selector(|| "rename-vault-error".to_owned())
                .font_family("Inter")
                .text_size(px(10.))
                .line_height(relative(1.45))
                .text_color(rgb(0xC9959A))
                .child("A vault needs a name.")
        });

        Some(
            div()
                .id("rename-vault-layer")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x0A0C0F99))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.cancel_rename_vault(window, cx)),
                )
                .child(
                    div()
                        .id("rename-vault-dialog")
                        .debug_selector(|| "rename-vault-dialog".to_owned())
                        .w(px(440.))
                        .flex()
                        .flex_col()
                        .gap(px(16.))
                        .p(px(21.))
                        .rounded(px(12.))
                        .border_1()
                        .border_color(rgb(0x3A4450))
                        .bg(rgb(0x202630))
                        .track_focus(&dialog_focus)
                        .role(gpui::Role::Dialog)
                        .aria_label(format!("Rename {}", dialog.original))
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            match event.keystroke.key.as_str() {
                                "escape" => {
                                    window.prevent_default();
                                    this.cancel_rename_vault(window, cx);
                                }
                                // Enter submits from the field, the way every
                                // other form in the app behaves.
                                "enter" => {
                                    window.prevent_default();
                                    this.confirm_rename_vault(window, cx);
                                }
                                _ => {}
                            }
                        }))
                        .on_mouse_down(MouseButton::Left, |_, _, app| app.stop_propagation())
                        .child(
                            div()
                                .debug_selector(|| "rename-vault-title".to_owned())
                                .font_family("Inter")
                                .text_size(px(15.))
                                .line_height(relative(1.2))
                                .font_weight(FontWeight(650.))
                                .text_color(rgb(0xF3F5F7))
                                .child("Rename vault"),
                        )
                        .child(
                            div()
                                .debug_selector(|| "rename-vault-body".to_owned())
                                .w_full()
                                .font_family("Inter")
                                .text_size(px(11.))
                                .line_height(relative(1.45))
                                .text_color(rgb(0xB9C0C8))
                                .child(
                                    "Only the label changes. The vault file stays where it is, \
                                     so its folder keeps its original name.",
                                ),
                        )
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .flex_col()
                                .gap(px(7.))
                                .child(
                                    div()
                                        .font_family("Inter")
                                        .text_size(px(9.))
                                        .font_weight(FontWeight(700.))
                                        .text_color(rgb(0x7F8996))
                                        .child("VAULT NAME"),
                                )
                                .child(
                                    Input::new(&dialog.input)
                                        .prefix(
                                            gpui_component::Icon::empty()
                                                .path("icons/database.svg")
                                                .size(px(14.))
                                                .text_color(rgb(0x737E8D)),
                                        )
                                        .aria_label("Vault name"),
                                )
                                .children(error),
                        )
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap(px(8.))
                                .child(cancel)
                                .child(confirm),
                        )
                        .focus_trap("rename-vault-focus-trap", &dialog_focus),
                )
                .into_any_element(),
        )
    }

    fn render_remove_vault_dialog(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let dialog = self.remove_vault_dialog.clone()?;
        let checked = dialog.delete_files;
        let warning = if checked {
            "Every password in this vault is destroyed. Nox has no password recovery and this \
             cannot be undone."
        } else {
            "Leave this off to keep the file — useful for a vault on a drive you unplug."
        };
        let option_fill = if checked { 0x2A2024 } else { 0x1B2029 };
        let option_border = if checked { 0xA9787D } else { 0x343D48 };
        let option_label = if checked { 0xE9C3C6 } else { 0xDDE3E8 };
        let option_warning = if checked { 0xC9959A } else { 0x7F8996 };
        let confirm_fill = if checked { 0xA9787D } else { 0xE3E6ED };
        let confirm_label = if checked { "Delete vault" } else { "Remove" };

        let option_focus = self.remove_vault_option_focus.clone();
        let cancel_focus = self.remove_vault_cancel_focus.clone();
        let confirm_focus = self.remove_vault_confirm_focus.clone();
        let dialog_focus = self.remove_vault_dialog_focus.clone();
        let toggle_for_a11y = cx.entity();
        let cancel_for_a11y = cx.entity();
        let confirm_for_a11y = cx.entity();

        let delete_option = dialog.file_exists.then(|| {
            div()
                .id("remove-vault-delete-files")
                .debug_selector(|| "remove-vault-delete-files".to_owned())
                .w_full()
                .flex()
                .items_start()
                .gap(px(10.))
                .p(px(11.))
                .rounded(px(8.))
                .border_1()
                .border_color(rgb(option_border))
                .bg(rgb(option_fill))
                .cursor_pointer()
                .track_focus(&option_focus)
                .role(gpui::Role::CheckBox)
                .aria_label("Also delete the vault file permanently")
                .aria_description(warning)
                .aria_toggled(if checked {
                    gpui::Toggled::True
                } else {
                    gpui::Toggled::False
                })
                .focus_visible(|style| style.border_color(rgb(0x77B8DF)))
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        window.prevent_default();
                        this.toggle_remove_vault_files(cx);
                    }
                }))
                .on_click(cx.listener(|this, _, _, cx| this.toggle_remove_vault_files(cx)))
                .on_a11y_action(gpui::AccessibleAction::Click, move |_, _, app| {
                    toggle_for_a11y.update(app, |this, cx| {
                        this.toggle_remove_vault_files(cx);
                    });
                })
                .child(
                    div()
                        .flex_shrink_0()
                        .size(px(16.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .border_1()
                        .border_color(rgb(if checked { 0xA9787D } else { 0x3A4450 }))
                        .bg(rgb(if checked { 0xA9787D } else { 0x202630 }))
                        .when(checked, |checkbox| {
                            checkbox.child(
                                gpui_component::Icon::empty()
                                    .path("icons/check.svg")
                                    .size(px(11.))
                                    .text_color(rgb(0x12161C)),
                            )
                        }),
                )
                .child(
                    div()
                        .min_w(px(0.))
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap(px(5.))
                        .child(
                            div()
                                .debug_selector(|| "remove-vault-option-label".to_owned())
                                .font_family("Inter")
                                .text_size(px(11.))
                                .line_height(relative(1.18))
                                .font_weight(FontWeight(550.))
                                .text_color(rgb(option_label))
                                .child("Also delete the vault file permanently"),
                        )
                        .child(
                            div()
                                .debug_selector(|| "remove-vault-option-warning".to_owned())
                                .font_family("Inter")
                                .text_size(px(10.))
                                .line_height(px(15.))
                                .when(!checked, |text| text.whitespace_nowrap())
                                .text_color(rgb(option_warning))
                                .child(warning),
                        ),
                )
        });

        let cancel = div()
            .id("remove-vault-cancel")
            .debug_selector(|| "remove-vault-cancel".to_owned())
            .w(px(69.))
            .h(px(34.))
            .px(px(16.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(7.))
            .border_1()
            .border_color(rgb(0x3A4450))
            .bg(rgb(0x222731))
            .font_family("Inter")
            .text_size(px(11.))
            .font_weight(FontWeight(550.))
            .text_color(rgb(0xDDE3E8))
            .cursor_pointer()
            .track_focus(&cancel_focus)
            .role(gpui::Role::Button)
            .aria_label("Cancel")
            .focus_visible(|style| style.border_color(rgb(0x77B8DF)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    window.prevent_default();
                    this.cancel_remove_vault(window, cx);
                }
            }))
            .on_click(cx.listener(|this, _, window, cx| {
                this.cancel_remove_vault(window, cx);
            }))
            .on_a11y_action(gpui::AccessibleAction::Click, move |_, window, app| {
                cancel_for_a11y.update(app, |this, cx| {
                    this.cancel_remove_vault(window, cx);
                });
            })
            .child("Cancel");

        let confirm = div()
            .id("remove-vault-confirm")
            .debug_selector(|| "remove-vault-confirm".to_owned())
            .w(px(if checked { 94. } else { 74. }))
            .h(px(34.))
            .px(px(16.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(7.))
            .border_1()
            .border_color(rgb(confirm_fill))
            .bg(rgb(confirm_fill))
            .font_family("Inter")
            .text_size(px(11.))
            .font_weight(FontWeight(650.))
            .text_color(rgb(0x1A1D22))
            .cursor_pointer()
            .track_focus(&confirm_focus)
            .role(gpui::Role::Button)
            .aria_label(confirm_label)
            .focus_visible(|style| style.border_color(rgb(0x77B8DF)))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                    window.prevent_default();
                    this.confirm_remove_vault(window, cx);
                }
            }))
            .on_click(cx.listener(|this, _, window, cx| {
                this.confirm_remove_vault(window, cx);
            }))
            .on_a11y_action(gpui::AccessibleAction::Click, move |_, window, app| {
                confirm_for_a11y.update(app, |this, cx| {
                    this.confirm_remove_vault(window, cx);
                });
            })
            .child(confirm_label);

        Some(
            div()
                .id("remove-vault-modal-layer")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(0x0A0C0F99))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        this.cancel_remove_vault(window, cx);
                    }),
                )
                .child(
                    div()
                        .id("remove-vault-dialog")
                        .debug_selector(|| "remove-vault-dialog".to_owned())
                        .w(px(440.))
                        .flex()
                        .flex_col()
                        .gap(px(16.))
                        .p(px(21.))
                        .rounded(px(12.))
                        .border_1()
                        .border_color(rgb(0x3A4450))
                        .bg(rgb(0x202630))
                        .track_focus(&dialog_focus)
                        .role(gpui::Role::Dialog)
                        .aria_label(format!("Remove {} from the list?", dialog.name))
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            if event.keystroke.key.as_str() == "escape" {
                                window.prevent_default();
                                this.cancel_remove_vault(window, cx);
                            }
                        }))
                        .on_mouse_down(MouseButton::Left, |_, _, app| app.stop_propagation())
                        .child(
                            div()
                                .debug_selector(|| "remove-vault-title".to_owned())
                                .font_family("Inter")
                                .text_size(px(15.))
                                .line_height(relative(1.2))
                                .font_weight(FontWeight(650.))
                                .text_color(rgb(0xF3F5F7))
                                .child(format!("Remove “{}” from the list?", dialog.name)),
                        )
                        .child(
                            div()
                                .debug_selector(|| "remove-vault-body".to_owned())
                                .w_full()
                                .font_family("Inter")
                                .text_size(px(11.))
                                .line_height(relative(1.45))
                                .text_color(rgb(0xB9C0C8))
                                .child(
                                    "Nox stops listing this vault. Its file stays on disk, so you \
                                     can open it again later.",
                                ),
                        )
                        .children(delete_option)
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .items_center()
                                .justify_end()
                                .gap(px(8.))
                                .child(cancel)
                                .child(confirm),
                        )
                        .focus_trap("remove-vault-focus-trap", &dialog_focus),
                )
                .into_any_element(),
        )
    }

    /// Rebuild the picker's delegate from the registry.
    ///
    /// A stale delegate keeps offering a vault that is no longer registered,
    /// and its selected index points into the old list.
    fn sync_vault_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entries = self.vaults.vaults.clone();
        let selected = self
            .active_vault
            .as_ref()
            .and_then(|active| entries.iter().position(|vault| vault.id == active.id))
            .map(IndexPath::new);
        self.vault_select.update(cx, |select, cx| {
            select.set_items(VaultDelegate(entries), window, cx);
            select.set_selected_index(selected, window, cx);
        });
    }

    fn begin_create_from_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state = AppState::NoVault;
        self.create_state = FormState::Idle;
        self.reset_create_inputs(window, cx);
        Self::focus_input(&self.create_name, window, cx);
        cx.notify();
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
        let name = self.create_name.read(cx).value();
        let name = if name.trim().is_empty() {
            "Personal vault".to_string()
        } else {
            name.to_string()
        };
        let path = self.new_vault_path(&name);
        let created_entry = VaultEntry::new(name, path.clone());
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
                            this.active_vault = Some(created_entry.clone());
                            this.vaults.vaults.push(created_entry.clone());
                            this.record_vault_opened(&created_entry);
                            if let Err(error) = save_registry(&this.data_dir, &this.vaults) {
                                eprintln!("vault registry persistence failed: {error}");
                            }
                            this.state = AppState::Unlocked(vault);
                            crate::theme::apply(ThemeMode::Dark, Some(window), cx);
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
        let Some(path) = self.vault_path().map(Path::to_path_buf) else {
            return;
        };

        let password = self.unlock_password.read(cx).value();
        let secret = SecretBytes::new(password.as_bytes());
        self.reset_unlock_input(window, cx);
        self.unlock_state = FormState::Pending;
        cx.notify();
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
                            if let Some(entry) = this.active_vault.clone() {
                                this.record_vault_opened(&entry);
                                if let Err(error) = save_registry(&this.data_dir, &this.vaults) {
                                    eprintln!("vault registry persistence failed: {error}");
                                }
                            }
                            this.state = AppState::Unlocked(vault);
                            crate::theme::apply(ThemeMode::Dark, Some(window), cx);
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

    pub(crate) fn note_activity(&mut self, cx: &mut Context<Self>) {
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
        crate::theme::apply(ThemeMode::Dark, Some(window), cx);
        Self::focus_input(&self.unlock_password, window, cx);
        cx.notify();
    }

    fn run_window_command(
        &mut self,
        command: WindowCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            WindowCommand::Minimize => window.minimize_window(),
            WindowCommand::ToggleMaximize => window.zoom_window(),
            WindowCommand::NewVault => {
                if matches!(&self.state, AppState::Unlocked(_)) {
                    self.lock_vault(window, cx);
                }
                self.begin_create_from_picker(window, cx);
            }
            WindowCommand::OpenVault => self.return_to_unlock(window, cx),
            WindowCommand::LockVault => self.lock_vault(window, cx),
            WindowCommand::Close => window.remove_window(),
        }
    }

    fn render_no_vault(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = Theme::current(cx);
        let pending = self.create_state == FormState::Pending;
        let backup_busy = !self.backup.is_idle();
        let error = match &self.create_state {
            FormState::Error(message) => div()
                .text_sm()
                .text_center()
                .text_color(theme.danger)
                .child(message.clone()),
            FormState::Idle | FormState::Pending => div(),
        };
        let backup_status = match &self.backup.operation {
            backup::BackupOperation::Failed(message) => div()
                .text_sm()
                .text_center()
                .text_color(theme.danger)
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
            (
                theme.inverse,
                theme.inverse_hover,
                theme.inverse_active,
                theme.on_inverse,
            ),
            cx,
        );
        let restore_button = Button::new("create-restore-backup")
            .w_full()
            .h(px(32.))
            .px(px(12.))
            .disabled(backup_busy)
            .on_click(cx.listener(|this, _, window, cx| this.begin_restore(window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .w_full()
                    .gap(px(7.))
                    .child(
                        gpui_component::Icon::empty()
                            .path("icons/archive-restore.svg")
                            .size(px(14.))
                            .text_color(theme.text_subtle),
                    )
                    .child("Restore a backup instead"),
            );
        let restore_button = animated_auth_button(
            "create-restore-backup",
            restore_button,
            self.auth_hovered.get("create-restore-backup").copied(),
            (
                theme.canvas,
                theme.raised,
                theme.border,
                theme.text_secondary,
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
                bg={theme.canvas}
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
                        {logo(52., theme.text)}
                        <div flex flex_col items_center gap={px(4.)}>
                            <div text_lg fontWeight={FontWeight::SEMIBOLD} textColor={theme.text}>
                                {"Create your vault"}
                            </div>
                            <div text_xs text_center textColor={theme.text_muted}>
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
                        bg={theme.surface}
                        border_1
                        borderColor={theme.border}
                    >
                        <div size={px(28.)} flex items_center justify_center rounded_full bg={theme.raised}>
                            <div text_sm fontWeight={FontWeight::BOLD} textColor={theme.text}>{"!"}</div>
                        </div>
                        <div flex flex_col flex_1 gap={px(2.)}>
                            <div text_sm fontWeight={FontWeight::SEMIBOLD} textColor={theme.text_soft}>
                                {"No password recovery"}
                            </div>
                            <div text_xs textColor={theme.text_subtle}>
                                {"Store your master password somewhere safe"}
                            </div>
                        </div>
                        {gpui_component::Icon::empty()
                            .path("icons/shield-alert.svg")
                            .size(px(14.))
                            .text_color(theme.text_subtle)}
                    </div>
                    <div id="create-vault-name" flex flex_col gap={px(7.)}>
                        <div text_xs fontWeight={FontWeight::BOLD} textColor={theme.text_subtle}>
                            {"VAULT NAME"}
                        </div>
                            <Input
                                base={Input::new(&self.create_name)
                                    .prefix(
                                    gpui_component::Icon::empty()
                                        .path("icons/database.svg")
                                        .size(px(15.))
                                        .text_color(theme.icon_muted),
                                    )
                                    .aria_label("Vault name")}
                                h={px(44.)}
                                bg={theme.surface}
                                borderColor={theme.border_strong}
                                rounded={px(8.)}
                        />
                    </div>
                    <div id="create-vault-password" flex flex_col gap={px(7.)}>
                        <div text_xs fontWeight={FontWeight::BOLD} textColor={theme.text_subtle}>
                            {"MASTER PASSWORD"}
                        </div>
                        <Input
                            base={Input::new(&self.create_password)
                                .mask_toggle()
                                .prefix(
                                    gpui_component::Icon::empty()
                                        .path("icons/lock.svg")
                                        .size(px(15.))
                                        .text_color(theme.icon_muted),
                                )
                                .aria_label("Master password")}
                            h={px(44.)}
                            bg={theme.surface}
                            borderColor={theme.border_strong}
                            rounded={px(8.)}
                        />
                    </div>
                    <div id="create-vault-confirm" flex flex_col gap={px(7.)}>
                        <div text_xs fontWeight={FontWeight::BOLD} textColor={theme.text_subtle}>
                            {"CONFIRM MASTER PASSWORD"}
                        </div>
                        <Input
                            base={Input::new(&self.create_confirm)
                                .mask_toggle()
                                .prefix(
                                    gpui_component::Icon::empty()
                                        .path("icons/lock.svg")
                                        .size(px(15.))
                                        .text_color(theme.icon_muted),
                                )
                                .aria_label("Confirm master password")}
                            h={px(44.)}
                            bg={theme.surface}
                            borderColor={theme.border_strong}
                            rounded={px(8.)}
                        />
                    </div>
                    {error}
                    {create_button}
                    <div flex flex_col gap={px(8.)}>
                        <div h={px(1.)} w_full bg={theme.border} />
                        {if self.vaults.vaults.is_empty() {
                            div().into_any_element()
                        } else {
                            Button::new("create-back-to-vault-list")
                                .h(px(32.))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.return_to_unlock(window, cx);
                                }))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(7.))
                                        .child("Back"),
                                )
                                .into_any_element()
                        }}
                        {restore_button}
                        {backup_status}
                        <div text_xs text_center textColor={theme.text_ghost}>
                            {"Enter to create · Esc to close"}
                        </div>
                    </div>
                    <div flex items_center justify_center gap={px(7.)} textColor={theme.text_subtle}>
                        {gpui_component::Icon::empty().path("icons/shield-check.svg").size(px(13.))}
                        <div text_xs>{"Encrypted locally · You hold the keys"}</div>
                    </div>
                </div>
            </div>
        }
    }

    fn render_locked(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current(cx);
        let pending = self.unlock_state == FormState::Pending;
        let backup_busy = !self.backup.is_idle();
        let trigger_content = self.active_vault.as_ref().map_or_else(
            || div().child("Select a vault").into_any_element(),
            |vault| {
                vault_row_content(
                    theme,
                    vault,
                    Some(
                        gpui_component::Icon::empty()
                            .path("icons/chevron-down.svg")
                            .size(px(14.))
                            .text_color(theme.icon_muted)
                            .into_any_element(),
                    ),
                )
            },
        );
        let trigger_variant = ButtonCustomVariant::new(cx)
            .color(theme.surface)
            .hover(theme.surface)
            .active(theme.surface)
            .foreground(theme.text_soft);
        let trigger = Button::new("vault-select-trigger")
            .custom(trigger_variant)
            .w_full()
            .h(px(48.))
            .px(px(12.))
            .rounded(px(8.))
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface)
            .child(trigger_content);
        let vaults = self.vaults.vaults.clone();
        let selected_id = self.active_vault.as_ref().map(|vault| vault.id.clone());
        let select = self.vault_select.clone();
        let vault_select = Popover::new("vault-select-popover")
            .appearance(false)
            .trigger(trigger)
            .content(move |_state, _window, cx| {
                let popover = cx.entity();
                let rows = vaults.iter().cloned().enumerate().map(|(index, vault)| {
                    let missing = !vault.path.is_file();
                    let checked = selected_id.as_ref() == Some(&vault.id);
                    let (icon, color) = vault_dropdown_trailing(theme, missing, checked);
                    let trailing = gpui_component::Icon::empty()
                        .path(icon)
                        .size(px(14.))
                        .text_color(color)
                        .into_any_element();
                    let select = select.clone();
                    let popover = popover.clone();
                    let id = vault.id.clone();
                    div()
                        .id(("vault-option", index))
                        .w_full()
                        .h(px(40.))
                        .px(px(8.))
                        .rounded(px(6.))
                        .bg(if checked { theme.raised } else { theme.surface })
                        .when(!checked && !missing, |row| {
                            row.hover(|style| style.bg(theme.raised))
                        })
                        .when(!missing, |row| {
                            row.cursor_pointer().on_click(move |_, window, app| {
                                select.update(app, |_, cx| {
                                    cx.emit(SelectEvent::Confirm(Some(id.clone())));
                                });
                                popover.update(app, |state, cx| state.dismiss(window, cx));
                            })
                        })
                        .child(vault_row_content(theme, &vault, Some(trailing)))
                });
                div()
                    .w(px(416.))
                    .p(px(4.))
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.surface)
                    .shadow(vec![BoxShadow {
                        color: rgba(0x00000066).into(),
                        offset: point(px(0.), px(4.)),
                        blur_radius: px(12.),
                        spread_radius: px(0.),
                        inset: false,
                    }])
                    .children(rows)
            });
        let error = match &self.unlock_state {
            FormState::Error(message) => div()
                .text_sm()
                .text_color(theme.danger)
                .child(message.clone()),
            FormState::Idle | FormState::Pending => div(),
        };
        let backup_status = match &self.backup.operation {
            backup::BackupOperation::Failed(message) => div()
                .text_sm()
                .text_center()
                .text_color(theme.danger)
                .child(message.clone()),
            backup::BackupOperation::Succeeded(message) => div()
                .text_sm()
                .text_center()
                .text_color(theme.text_subtle)
                .child(message.clone()),
            backup::BackupOperation::AwaitingRestoreConfirmation { archive_path } => div()
                .text_sm()
                .text_center()
                .text_color(theme.text_subtle)
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
                    .text_size(px(12.))
                    .font_weight(FontWeight::BOLD)
                    .child(
                        gpui_component::Icon::empty()
                            .path("icons/lock-keyhole-open.svg")
                            .size(px(15.))
                            .text_color(theme.on_inverse),
                    )
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
            (
                theme.inverse,
                theme.inverse_hover,
                theme.inverse_active,
                theme.on_inverse,
            ),
            cx,
        );
        let create_button = Button::new("locked-create-vault")
            .w_full()
            .h(px(32.))
            .px(px(12.))
            .on_click(cx.listener(|this, _, window, cx| this.begin_create_from_picker(window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .w_full()
                    .gap(px(7.))
                    .child(
                        gpui_component::Icon::empty()
                            .path("icons/plus.svg")
                            .size(px(14.))
                            .text_color(theme.text_subtle),
                    )
                    .child("Create a new vault"),
            );
        let create_button = animated_auth_button(
            "locked-create-vault",
            create_button,
            self.auth_hovered.get("locked-create-vault").copied(),
            (
                theme.canvas,
                theme.raised,
                theme.border,
                theme.text_secondary,
            ),
            cx,
        );
        let restore_button = Button::new("restore-backup")
            .w_full()
            .h(px(32.))
            .px(px(12.))
            .disabled(backup_busy)
            .on_click(cx.listener(|this, _, window, cx| this.begin_restore(window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .w_full()
                    .gap(px(7.))
                    .child(
                        gpui_component::Icon::empty()
                            .path("icons/archive-restore.svg")
                            .size(px(14.))
                            .text_color(theme.text_subtle),
                    )
                    .child("Restore a backup instead"),
            );
        let restore_button = animated_auth_button(
            "restore-backup",
            restore_button,
            self.auth_hovered.get("restore-backup").copied(),
            (
                theme.canvas,
                theme.raised,
                theme.border,
                theme.text_secondary,
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
                bg={theme.canvas}
                p={px(36.)}
                onKeyDown={cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    match event.keystroke.key.as_str() {
                        "enter" => this.unlock_vault(window, cx),
                        "escape" => window.remove_window(),
                        _ => {}
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
                        {logo(52., theme.text)}
                        <div flex flex_col items_center gap={px(4.)}>
                            <div text_lg fontWeight={FontWeight::SEMIBOLD} textColor={theme.text}>
                                {"Unlock your vault"}
                            </div>
                            <div text_xs text_center textColor={theme.text_muted}>
                                {"Enter your master password to continue"}
                            </div>
                        </div>
                    </div>
                    <div id="locked-vault-summary">
                        {vault_select}
                    </div>
                    <div id="unlock-password" flex flex_col gap={px(7.)}>
                        <div text_xs fontWeight={FontWeight::BOLD} textColor={theme.text_subtle}>
                            {"MASTER PASSWORD"}
                        </div>
                        <Input
                            base={Input::new(&self.unlock_password)
                                .large()
                                .focus_bordered(false)
                                .text_size(px(11.))
                                .gap(px(10.))
                                .mask_toggle()
                                .prefix(
                                    gpui_component::Icon::empty()
                                        .path("icons/lock.svg")
                                        .size(px(15.))
                                        .text_color(theme.icon_muted),
                                )
                                .aria_label("Master password")}
                            h={px(44.)}
                            bg={theme.surface}
                            borderColor={theme.border_strong}
                            rounded={px(8.)}
                        />
                    </div>
                    {error}
                    {unlock_button}
                    <div flex flex_col gap={px(8.)}>
                        <div h={px(1.)} w_full bg={theme.border} />
                        {create_button}
                        {restore_button}
                        {backup_status}
                        <div text_xs text_center textColor={theme.text_ghost}>
                            {"Enter to unlock · Esc to close"}
                        </div>
                    </div>
                    <div flex items_center justify_center gap={px(7.)} textColor={theme.text_subtle}>
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
        let theme = Theme::current(cx);
        if self.active_view == ActiveView::Home {
            let nav = self.render_sidebar_nav(cx);
            let home = self.render_home(window, cx);
            return rsx! { <div id="home-shell" size_full flex bg={theme.canvas}>{nav}{home}</div> };
        }
        if self.uses_secure_note_workspace() {
            let nav = self.render_sidebar_nav(cx);
            let workspace = self.render_secure_note_workspace(window, cx);
            return rsx! {
                <div id="secure-note-workspace-shell" size_full flex bg={SECURE_NOTE_PAPER}>
                    {nav}
                    {workspace}
                </div>
            };
        }
        if self.uses_login_workspace() {
            let nav = self.render_sidebar_nav(cx);
            let workspace = self.render_login_workspace(window, cx);
            return rsx! {
                <div id="login-workspace-shell" size_full flex bg={theme.canvas}>
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
                .filter(|(_, item)| item.item_type == nox_core::ItemType::Login)
                .count();
            let notes = list
                .items
                .iter()
                .filter(|(_, item)| item.item_type == nox_core::ItemType::SecureNote)
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
                bg={theme.canvas}
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
                        borderColor={theme.border}
                    >
                        <div flex flex_col gap={px(3.)}>
                            <div text_xl fontWeight={FontWeight::SEMIBOLD} textColor={theme.text}>{page_title}</div>
                            <div text_xs textColor={theme.text_muted}>
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
                                    .icon(gpui_component::Icon::empty().path("icons/lock-keyhole-open.svg").text_color(theme.text_muted))
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
                            {sync_status_pill(theme)}
                            {add}
                        </div>
                    </div>
                    {conflict_panel}
                    <div flex flex_col flex_1 min_h={px(0.)} p={px(28.)} pt={px(20.)} gap={px(16.)} bg={theme.canvas}>
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
        let theme = Theme::current(cx);
        let (total, logins, notes, recent) =
            self.vault_list
                .as_ref()
                .map_or((0, 0, 0, Vec::new()), |list| {
                    let logins = list
                        .items
                        .iter()
                        .filter(|(_, item)| item.item_type == nox_core::ItemType::Login)
                        .count();
                    let notes = list
                        .items
                        .iter()
                        .filter(|(_, item)| item.item_type == nox_core::ItemType::SecureNote)
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
            .text_color(theme.text_secondary)
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
                let is_login = item.item_type == nox_core::ItemType::Login;
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
                    .border_color(theme.border)
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
                                            .bg(theme.raised)
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .child(
                                                gpui_component::Icon::empty()
                                                    .path(icon_path)
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
                                                div().text_sm().text_color(theme.text).child(title),
                                            )
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(theme.text_muted)
                                                    .child(subtitle),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(16.))
                                    .child(div().text_xs().text_color(theme.text_muted).child(time))
                                    .child(
                                        gpui_component::Icon::empty()
                                            .path("icons/ellipsis-vertical.svg")
                                            .size(px(14.))
                                            .text_color(theme.text_muted),
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
                .text_color(theme.text_muted)
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
            <div id="home-workspace" flex flex_col flex_1 min_w={px(0.)} h_full bg={theme.canvas}>
                <div id="home-header" flex items_center justify_between h={px(88.)} px={px(32.)} flex_shrink_0 border_b_1 borderColor={theme.border}>
                    <div flex flex_col gap={px(3.)}>
                        <div text_xl fontWeight={FontWeight::SEMIBOLD} textColor={theme.text}>{"Home"}</div>
                        <div text_xs textColor={theme.text_muted}>{"Your vault at a glance"}</div>
                    </div>
                    <div flex items_center gap={px(10.)}>
                        {sync_status_pill(theme)}
                        {add}
                    </div>
                </div>
                <div id="home-dashboard" flex flex_col gap={px(20.)} p={px(32.)} overflow_y_scroll>
                    <div id="home-hero" flex items_start justify_between p={px(24.)} bg={theme.surface} rounded={px(10.)} border_1 border_color={theme.field_border}>
                        <div flex flex_col gap={px(8.)} w={px(360.)}>
                            <div text_xs fontWeight={FontWeight::BOLD} textColor={theme.text_muted}>{"WELCOME BACK"}</div>
                            <div text_lg fontWeight={FontWeight::SEMIBOLD} textColor={theme.text}>{"Your vault is secure"}</div>
                            <div text_sm textColor={theme.text_muted}>
                                {format!(
                                    "Stored locally and encrypted. {total} item{} in your vault.",
                                    if total == 1 { "" } else { "s" },
                                )}
                            </div>
                        </div>
                        <div flex gap={px(12.)}>
                            {home_stat_tile(theme, "VAULT ITEMS", total)}
                            {home_stat_tile(theme, "LOGINS", logins)}
                            {home_stat_tile(theme, "SECURE NOTES", notes)}
                        </div>
                    </div>
                    <div id="home-quick-actions" flex flex_col gap={px(12.)}>
                        <div flex items_center justify_between>
                            <div text_sm fontWeight={FontWeight::SEMIBOLD} textColor={theme.text}>{"Quick actions"}</div>
                            <div text_xs textColor={theme.text_muted}>{"Ctrl+P to search or create"}</div>
                        </div>
                        <div flex gap={px(12.)}>
                            {new_login}
                            {new_note}
                            {new_card}
                            {new_identity}
                        </div>
                    </div>
                    <div flex gap={px(20.)} items_start>
                        <div id="home-recent-items" flex flex_col flex_1 min_w={px(0.)} rounded={px(10.)} bg={theme.surface}>
                            <div flex items_center justify_between h={px(58.)} px={px(18.)} border_b_1 borderColor={theme.border}>
                                <div flex flex_col gap={px(2.)}>
                                    <div text_sm fontWeight={FontWeight::SEMIBOLD} textColor={theme.text}>{"Recent items"}</div>
                                    <div text_xs textColor={theme.text_muted}>{"Newest first"}</div>
                                </div>
                                {view_all}
                            </div>
                            {recent_body}
                        </div>
                        <div flex flex_col gap={px(16.)} w={px(330.)} flex_shrink_0>
                            <div flex flex_col gap={px(10.)} p={px(18.)} rounded={px(10.)} bg={theme.surface}>
                                <div flex items_center gap={px(8.)}>
                                    {gpui_component::Icon::empty().path("icons/shield-check.svg").size(px(15.)).text_color(theme.text_secondary)}
                                    <div text_sm fontWeight={FontWeight::SEMIBOLD} textColor={theme.text}>{"Security health"}</div>
                                </div>
                                <div text_xs textColor={theme.text_muted}>{"Password health scoring isn't available yet."}</div>
                            </div>
                            <div flex flex_col gap={px(10.)} p={px(18.)} rounded={px(10.)} bg={theme.surface}>
                                <div flex items_center gap={px(8.)}>
                                    {gpui_component::Icon::empty().path("icons/star.svg").size(px(15.)).text_color(theme.text_secondary)}
                                    <div text_sm fontWeight={FontWeight::SEMIBOLD} textColor={theme.text}>{"Favorites"}</div>
                                </div>
                                <div text_xs textColor={theme.text_muted}>{"Favoriting items isn't available yet."}</div>
                            </div>
                        </div>
                    </div>
                </div>
            </div>
        }.into_any_element()
    }
}

impl Render for Nox {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::current(cx);
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
                AppState::RegistryError => div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(8.))
                    .p(px(32.))
                    .text_color(theme.text)
                    .child("Nox could not read vaults.json.")
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.text_muted)
                            .child("The file was left unchanged. Fix it, then restart Nox."),
                    )
                    .into_any_element(),
                AppState::Locked => self.render_locked(window, cx).into_any_element(),
                AppState::Unlocked(_) => self.render_unlocked(window, cx).into_any_element(),
            }
        };
        let dialog_layer = Root::render_dialog_layer(window, cx);
        let sheet_layer = Root::render_sheet_layer(window, cx);
        let settings_modal = self.settings_open.then(|| {
            settings::render_settings_modal(
                theme,
                cx.entity(),
                self.settings.clone(),
                self.settings_section,
                &settings::VaultListModel {
                    entries: self.vaults.vaults.clone(),
                    active_id: self.active_vault.as_ref().map(|vault| vault.id.clone()),
                },
                self.conflicts.count(),
            )
        });
        let remove_vault_modal = self.render_remove_vault_dialog(cx);
        let rename_vault_modal = self.render_rename_vault_dialog(cx);
        rsx! {
            <div
                size_full
                relative
                flex
                flex_col
                bg={theme.canvas}
                onAction={cx.listener(|this, _: &OpenCommandPalette, window, cx| {
                    // open_palette itself no-ops while unauthenticated.
                    this.window_controls
                        .update(cx, |controls, cx| controls.open_palette(window, cx));
                })}
            >
                {self.window_controls.clone()}
                <div flex_1 bg={theme.canvas}>{body}</div>
                {for modal in settings_modal {
                    {modal}
                }}
                {for modal in remove_vault_modal {
                    {modal}
                }}
                {for modal in rename_vault_modal {
                    {modal}
                }}
                {for dialog in dialog_layer {
                    {dialog}
                }}
                {for sheet in sheet_layer {
                    {sheet}
                }}
                {crate::ui::window::controls::resize_handles()}
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
        eprintln!("could not determine Nox data directory: {error}");
        Self {
            message: "Nox could not determine a safe data directory.".into(),
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
    use super::*;
    use crate::item_editor::EditorMode;
    use crate::{conflicts, vault_list};

    use crate::vaults::{VaultEntry, VaultRegistry, registry_path, save_registry};
    use gpui::{Focusable, TestAppContext, VisualTestContext};
    use gpui_component::{ActiveTheme, Root, Theme, ThemeMode, WindowExt};
    use nox_core::{
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
        let _ = gpui_rsx::rsx! { <div>{"Nox"}</div> };
    }

    fn test_path(label: &str) -> PathBuf {
        let id = NEXT_PATH.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir()
            .join(format!("locker-gui-{label}-{}-{id}", std::process::id()))
            .join("vault.db");
        let _ = fs::remove_dir_all(path.parent().unwrap());
        path
    }

    fn test_dir(label: &str) -> PathBuf {
        test_path(label).parent().unwrap().to_path_buf()
    }

    fn vault_file(data_dir: &Path, slug: &str) -> PathBuf {
        data_dir.join("vaults").join(slug).join("vault.db")
    }

    /// Register `names` as real (empty) vault files under `data_dir`, at the
    /// same slugged path `create_vault` would use.
    fn register_vaults(data_dir: &Path, names: &[&str]) {
        fs::create_dir_all(data_dir).unwrap();
        let entries: Vec<VaultEntry> = names
            .iter()
            .map(|name| {
                let slug = slug_for(name, &[]);
                let path = vault_file(data_dir, &slug);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, []).unwrap();
                VaultEntry::new(*name, path)
            })
            .collect();
        let registry = VaultRegistry {
            vaults: entries,
            ..VaultRegistry::default()
        };
        save_registry(data_dir, &registry).unwrap();
    }

    fn init(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
    }

    fn set_input(view: &Entity<Nox>, cx: &mut VisualTestContext, password: &str) {
        view.update_in(cx, |locker, window, locker_cx| {
            let input = locker.unlock_password.clone();
            input.update(locker_cx, |input, input_cx| {
                input.set_value(password.to_owned(), window, input_cx);
            });
        });
    }

    fn set_create_inputs(
        view: &Entity<Nox>,
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
    ) -> (Entity<Nox>, &mut VisualTestContext) {
        add_locker_view_with_clipboard_timeout(cx, path, timeout, DEFAULT_CLIPBOARD_TIMEOUT)
    }

    fn add_locker_view_with_clipboard_timeout(
        cx: &mut TestAppContext,
        path: PathBuf,
        inactivity_timeout: Duration,
        clipboard_timeout: Duration,
    ) -> (Entity<Nox>, &mut VisualTestContext) {
        add_nox_view_with_clipboard_timeout(
            cx,
            path.parent().unwrap().to_path_buf(),
            inactivity_timeout,
            clipboard_timeout,
        )
    }

    fn add_nox_view_with_clipboard_timeout(
        cx: &mut TestAppContext,
        data_dir: PathBuf,
        inactivity_timeout: Duration,
        clipboard_timeout: Duration,
    ) -> (Entity<Nox>, &mut VisualTestContext) {
        let holder = Rc::new(RefCell::new(None));
        let holder_for_window = holder.clone();
        let (_, visual_cx) = cx.add_window_view(move |window, cx| {
            let locker =
                cx.new(|cx| Nox::new(data_dir, inactivity_timeout, clipboard_timeout, window, cx));
            holder_for_window.borrow_mut().replace(locker.clone());
            Root::new(locker, window, cx)
        });
        (holder.take().unwrap(), visual_cx)
    }

    fn add_nox_view(
        cx: &mut TestAppContext,
        data_dir: PathBuf,
        timeout: Duration,
    ) -> (Entity<Nox>, &mut VisualTestContext) {
        add_nox_view_with_clipboard_timeout(cx, data_dir, timeout, DEFAULT_CLIPBOARD_TIMEOUT)
    }

    #[gpui::test]
    fn startup_routes_on_the_number_of_registered_vaults(cx: &mut TestAppContext) {
        init(cx);

        let empty = test_dir("route-empty");
        let (view, cx) = add_nox_view(cx, empty.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        assert!(view.read_with(cx, |nox, _| matches!(&nox.state, AppState::NoVault)));
        cleanup(&empty);

        let one = test_dir("route-one");
        register_vaults(&one, &["Personal vault"]);
        let (view, cx) = add_nox_view(cx, one.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        // A single-vault user must never see a picker.
        assert!(view.read_with(cx, |nox, _| matches!(&nox.state, AppState::Locked)));
        assert!(view.read_with(cx, |nox, _| nox.active_vault.is_some()));
        cleanup(&one);

        let many = test_dir("route-many");
        register_vaults(&many, &["Personal vault", "Work vault"]);
        let (view, cx) = add_nox_view(cx, many.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        // Several vaults still open straight to Locked — the inline Select on
        // its account row is the picker now, not a separate screen.
        assert!(view.read_with(cx, |nox, _| matches!(&nox.state, AppState::Locked)));
        assert!(view.read_with(cx, |nox, _| nox.active_vault.is_some()));
        cleanup(&many);
    }

    #[gpui::test]
    fn locked_screen_preselects_the_last_opened_vault_and_confirming_another_switches_it(
        cx: &mut TestAppContext,
    ) {
        init(cx);
        let dir = test_dir("inline-picker");
        register_vaults(&dir, &["Personal vault", "Work vault"]);
        {
            let mut registry = load_registry(&dir).registry;
            registry.last_opened = Some(registry.vaults[1].id.clone());
            save_registry(&dir, &registry).unwrap();
        }
        let (view, cx) = add_nox_view(cx, dir.clone(), DEFAULT_INACTIVITY_TIMEOUT);

        assert_eq!(
            view.read_with(cx, |nox, _| nox
                .active_vault
                .as_ref()
                .map(|v| v.name.clone())),
            Some("Work vault".into())
        );

        let personal_id = view.read_with(cx, |nox, _| nox.vaults.vaults[0].id.clone());
        view.update_in(cx, |nox, _window, cx| {
            let select = nox.vault_select.clone();
            select.update(cx, |_, cx| {
                cx.emit(SelectEvent::Confirm(Some(personal_id.clone())));
            });
        });
        cx.run_until_parked();

        assert_eq!(
            view.read_with(cx, |nox, _| nox
                .active_vault
                .as_ref()
                .map(|v| v.name.clone())),
            Some("Personal vault".into())
        );
        cleanup(&dir);
    }

    #[test]
    fn a_vault_with_a_missing_file_is_disabled_in_the_picker() {
        let dir = test_dir("disabled-check");
        fs::create_dir_all(&dir).unwrap();
        let present = dir.join("vault.db");
        fs::write(&present, []).unwrap();
        let missing = dir.join("gone.db");

        assert!(!VaultEntry::new("Personal vault", present).disabled());
        assert!(VaultEntry::new("Gone vault", missing).disabled());
        cleanup(&dir);
    }

    #[test]
    fn vault_dropdown_uses_a_check_for_the_selected_vault() {
        let theme = crate::theme::Theme::cipher_midnight();
        assert_eq!(
            vault_dropdown_trailing(theme, false, true),
            ("icons/check.svg", theme.text_soft)
        );
        assert_eq!(
            vault_dropdown_trailing(theme, false, false),
            ("icons/chevron-right.svg", theme.icon_muted)
        );
        assert_eq!(
            vault_dropdown_trailing(theme, true, false),
            ("icons/circle-x.svg", theme.text_ghost)
        );
    }

    #[gpui::test]
    fn remove_confirmation_is_an_in_app_modal(cx: &mut TestAppContext) {
        init(cx);
        let dir = test_dir("remove-confirmation-modal");
        register_vaults(&dir, &["Personal vault"]);
        let (view, cx) = add_nox_view(cx, dir.clone(), DEFAULT_INACTIVITY_TIMEOUT);

        view.update_in(cx, |nox, window, app| {
            nox.begin_remove_vault(0, window, app);
        });

        assert!(!cx.update(|window, app| window.has_active_dialog(app)));
        let dialog = cx.debug_bounds("remove-vault-dialog").unwrap();
        let option = cx.debug_bounds("remove-vault-delete-files").unwrap();
        let cancel = cx.debug_bounds("remove-vault-cancel").unwrap();
        let confirm = cx.debug_bounds("remove-vault-confirm").unwrap();
        assert_eq!(dialog.size, gpui::size(px(440.), px(233.)));
        assert_eq!(option.size, gpui::size(px(396.), px(57.)));
        assert_eq!(cancel.size, gpui::size(px(69.), px(34.)));
        assert_eq!(confirm.size, gpui::size(px(74.), px(34.)));
        assert!(view.read_with(cx, |nox, _| {
            nox.remove_vault_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.file_exists && !dialog.delete_files)
        }));

        view.update(cx, |nox, app| nox.toggle_remove_vault_files(app));
        let dialog = cx.debug_bounds("remove-vault-dialog").unwrap();
        let option = cx.debug_bounds("remove-vault-delete-files").unwrap();
        let confirm = cx.debug_bounds("remove-vault-confirm").unwrap();
        assert_eq!(dialog.size, gpui::size(px(440.), px(248.)));
        assert_eq!(option.size, gpui::size(px(396.), px(72.)));
        assert_eq!(confirm.size, gpui::size(px(94.), px(34.)));
        assert!(view.read_with(cx, |nox, _| {
            nox.remove_vault_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.delete_files)
        }));
        cleanup(&dir);
    }

    #[gpui::test]
    fn renaming_a_vault_only_touches_registry_metadata(cx: &mut TestAppContext) {
        init(cx);
        let dir = test_dir("rename-vault");
        register_vaults(&dir, &["Personal vault"]);
        let before = vault_file(&dir, "personal-vault");
        let (view, cx) = add_nox_view(cx, dir.clone(), DEFAULT_INACTIVITY_TIMEOUT);

        view.update_in(cx, |nox, window, app| {
            nox.rename_vault(0, "Renamed".into(), window, app)
        });

        assert_eq!(
            view.read_with(cx, |nox, _| nox.vaults.vaults[0].name.clone()),
            "Renamed"
        );
        // The database must not move: the slug directory keeps its original
        // name, and the file has to still be there.
        assert_eq!(
            view.read_with(cx, |nox, _| nox.vaults.vaults[0].path.clone()),
            before
        );
        assert!(before.exists());
        assert_eq!(load_registry(&dir).registry.vaults[0].name, "Renamed");
        cleanup(&dir);
    }

    #[gpui::test]
    fn renaming_trims_and_refuses_an_empty_name(cx: &mut TestAppContext) {
        init(cx);
        let dir = test_dir("rename-empty");
        register_vaults(&dir, &["Personal vault"]);
        let (view, cx) = add_nox_view(cx, dir.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        let name = |nox: &Nox| nox.vaults.vaults[0].name.clone();

        // Whitespace-only is refused outright rather than silently defaulted
        // to something the user never typed.
        assert!(!view.update_in(cx, |nox, window, app| {
            nox.rename_vault(0, "   ".into(), window, app)
        }));
        assert_eq!(view.read_with(cx, |nox, _| name(nox)), "Personal vault");

        // Surrounding whitespace is trimmed, not treated as a rejection.
        assert!(view.update_in(cx, |nox, window, app| {
            nox.rename_vault(0, "  Work vault  ".into(), window, app)
        }));
        assert_eq!(view.read_with(cx, |nox, _| name(nox)), "Work vault");
        cleanup(&dir);
    }

    #[gpui::test]
    fn renaming_the_active_vault_updates_the_locked_screen_picker(cx: &mut TestAppContext) {
        init(cx);
        let dir = test_dir("rename-active");
        register_vaults(&dir, &["Personal vault", "Work vault"]);
        let (view, cx) = add_nox_view(cx, dir.clone(), DEFAULT_INACTIVITY_TIMEOUT);

        view.update_in(cx, |nox, window, app| {
            nox.rename_vault(0, "Renamed".into(), window, app)
        });

        // A stale delegate would keep the old label on the trigger the user
        // is looking at while unlocking.
        let shown = view.read_with(cx, |nox, cx| {
            nox.vault_select
                .read(cx)
                .selected_value()
                .and_then(|id| nox.vaults.vaults.iter().find(|v| &v.id == id))
                .map(|v| v.name.clone())
        });
        assert_eq!(shown, Some("Renamed".into()));
        cleanup(&dir);
    }

    #[gpui::test]
    fn removing_a_vault_can_keep_or_delete_its_file(cx: &mut TestAppContext) {
        init(cx);
        let dir = test_dir("remove-vault");
        register_vaults(&dir, &["Personal vault", "Work vault"]);
        let kept = vault_file(&dir, "personal-vault");
        let deleted = vault_file(&dir, "work-vault");
        let (view, cx) = add_nox_view(cx, dir.clone(), DEFAULT_INACTIVITY_TIMEOUT);

        // Removing without the modifier is metadata-only.
        view.update_in(cx, |nox, window, app| {
            nox.remove_vault(1, false, window, app)
        });
        assert_eq!(view.read_with(cx, |nox, _| nox.vaults.vaults.len()), 1);
        assert!(deleted.exists());

        // The registry change is durable, not just in memory.
        assert_eq!(load_registry(&dir).registry.vaults.len(), 1);

        // Removing the last one with the modifier deletes it and empties the app.
        view.update_in(cx, |nox, window, app| {
            nox.remove_vault(0, true, window, app)
        });
        assert!(!kept.exists());
        assert!(view.read_with(cx, |nox, _| matches!(&nox.state, AppState::NoVault)));
        assert!(view.read_with(cx, |nox, _| nox.active_vault.is_none()));
        cleanup(&dir);
    }

    #[gpui::test]
    fn removing_the_active_vault_falls_back_to_a_surviving_one(cx: &mut TestAppContext) {
        init(cx);
        let dir = test_dir("remove-active");
        register_vaults(&dir, &["Personal vault", "Work vault"]);
        let (view, cx) = add_nox_view(cx, dir.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        // Startup makes the first entry active; removing it must hand the
        // Locked screen a different vault rather than leave it pointing at a
        // registry entry that no longer exists.
        assert_eq!(
            view.read_with(cx, |nox, _| nox
                .active_vault
                .as_ref()
                .map(|v| v.name.clone())),
            Some("Personal vault".into())
        );

        view.update_in(cx, |nox, window, app| {
            nox.remove_vault(0, true, window, app)
        });

        assert_eq!(
            view.read_with(cx, |nox, _| nox
                .active_vault
                .as_ref()
                .map(|v| v.name.clone())),
            Some("Work vault".into())
        );
        assert!(view.read_with(cx, |nox, _| matches!(&nox.state, AppState::Locked)));
        cleanup(&dir);
    }

    #[gpui::test]
    fn removing_the_unlocked_vault_locks_it_first(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("remove-unlocked");
        let vault = Vault::create(b"correct", &path).unwrap();
        let dir = path.parent().unwrap().to_path_buf();
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        view.update_in(cx, |nox, _window, _cx| {
            nox.state = AppState::Unlocked(vault);
        });

        view.update_in(cx, |nox, window, app| {
            nox.remove_vault(0, true, window, app)
        });

        // Deleting the file out from under an open vault without running the
        // existing teardown would leave its key material live in memory.
        assert!(view.read_with(cx, |nox, _| nox.vault_list.is_none()));
        assert!(view.read_with(cx, |nox, _| matches!(&nox.state, AppState::NoVault)));
        assert!(!path.exists());
        cleanup(&dir);
    }

    #[gpui::test]
    fn unreadable_registry_routes_to_an_error_without_overwriting_it(cx: &mut TestAppContext) {
        init(cx);
        let dir = test_dir("route-unreadable");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("vault.db"), []).unwrap();
        fs::write(registry_path(&dir), b"not json").unwrap();

        let (view, cx) = add_nox_view(cx, dir.clone(), DEFAULT_INACTIVITY_TIMEOUT);

        assert!(view.read_with(cx, |nox, _| matches!(&nox.state, AppState::RegistryError)));
        assert_eq!(fs::read(registry_path(&dir)).unwrap(), b"not json");
        cleanup(&dir);
    }

    #[gpui::test]
    fn fresh_and_existing_paths_select_the_expected_state(cx: &mut TestAppContext) {
        init(cx);
        let fresh = test_path("fresh");
        let (view, cx) = add_locker_view(cx, fresh.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        assert!(view.read_with(cx, |locker, _| matches!(&locker.state, AppState::NoVault)));
        // The vault-name field leads the create form, so it takes initial focus.
        let input = view.read_with(cx, |locker, _| locker.create_name.clone());
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
        // Creation now lands under `<data_dir>/vaults/<slug>/vault.db`, not the
        // flat legacy path the caller passed in.
        let created_path =
            view.read_with(cx, |locker, _| locker.vault_path().unwrap().to_path_buf());
        assert!(created_path.exists());
        // The vault landed under a nested `vaults/<slug>/` directory, not the flat
        // path `cleanup` expects — clean up the whole data dir instead.
        cleanup(path.parent().unwrap());
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
        let black: Hsla = gpui::rgb(0x000000).into();
        let white: Hsla = gpui::rgb(0xFFFFFF).into();
        assert_eq!(auth_hover_color(black, white, 0.), black);
        assert_eq!(auth_hover_color(black, white, 1.), white);
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
        // The vault-name field leads the create form, so it takes initial focus.
        let input = view.read_with(cx, |locker, _| locker.create_name.clone());
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
    fn locked_screens_create_action_opens_the_create_screen(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("locked-create-shortcut");
        write_existing(&path);
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);

        view.update_in(cx, |locker, window, locker_cx| {
            locker.begin_create_from_picker(window, locker_cx)
        });

        assert!(view.read_with(cx, |locker, _| matches!(&locker.state, AppState::NoVault)));
        let input = view.read_with(cx, |locker, _| locker.create_name.clone());
        assert!(cx.update(|window, app| { input.read(app).focus_handle(app).is_focused(window) }));
        cleanup(&path);
    }

    #[gpui::test]
    fn locked_page_escape_closes_the_window(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("locked-escape");
        write_existing(&path);
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
    ) -> (Entity<Nox>, &'a mut VisualTestContext, PathBuf, Vec<ItemId>) {
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
        view: &Entity<Nox>,
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
