use crate::backup::{self, BackupState};
use crate::clipboard::ClipboardState;
pub use crate::clipboard::DEFAULT_CLIPBOARD_TIMEOUT;
use crate::conflicts::ConflictState;
use crate::item_editor::{self, GeneratorPopoverState, ItemEditorState};
use crate::nav::ActiveView;
use crate::settings::{
    self, Settings, SettingsDurationDelegate, SettingsSection, load_settings, save_settings,
};
use crate::theme::{APP_FONT_FAMILY, Theme};
use crate::ui::window::controls::{OpenCommandPalette, WindowCommand, WindowControls};
use crate::vault_list::VaultListState;
use crate::vaults::{
    VaultEntry, VaultRegistry, adopt_legacy_vault, load_registry, save_registry, slug_for,
};

use gpui::{
    Animation, AnimationExt, AnyElement, Context, ElementId, Entity, FocusHandle, FontWeight, Hsla,
    KeyBinding, Render, Rgba, SharedString, Subscription, Task, Window, div, ease_out_quint,
    prelude::*, px,
};
use gpui_component::{
    Disableable, IndexPath, Root, ThemeMode, WindowExt,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    input::InputState,
    select::{SelectDelegate, SelectEvent, SelectItem, SelectState},
};
use gpui_rsx::rsx;
use nox_core::{SecretBytes, Vault, VaultError};
use std::{
    cell::RefCell,
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

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

pub(crate) fn vault_dropdown_trailing(
    theme: Theme,
    missing: bool,
    checked: bool,
) -> (&'static str, Hsla) {
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
pub(crate) fn vault_row_content(
    theme: Theme,
    vault: &VaultEntry,
    trailing: Option<AnyElement>,
) -> AnyElement {
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
    id: impl Into<SharedString>,
    button: Button,
    hovered: Option<bool>,
    colors: (Hsla, Hsla, Hsla, Hsla),
    cx: &mut Context<Nox>,
) -> AnyElement {
    let id: SharedString = id.into();
    let (base, hover, active, foreground) = colors;
    let variant = ButtonCustomVariant::new(cx)
        .foreground(foreground)
        .active(active);
    let button = button.on_hover(cx.listener({
        let id = id.clone();
        move |this, is_hovered, _, cx| {
            this.auth_hovered.insert(id.to_string(), *is_hovered);
            cx.notify();
        }
    }));
    let Some(hovered) = hovered else {
        return button
            .custom(variant.color(base).hover(base))
            // gpui-component's Custom variant mixes the *resting* bg 20% toward
            // transparent (button.rs `bg_color`), which washes any color out
            // against a dark canvas. An explicit `.bg()` wins over that
            // variant-computed style and keeps the resting fill solid.
            .bg(base)
            .into_any_element();
    };
    button
        .with_animation(
            ElementId::NamedInteger(id, u64::from(hovered)),
            Animation::new(AUTH_HOVER_DURATION).with_easing(ease_out_quint()),
            move |button, delta| {
                let amount = if hovered { delta } else { 1. - delta };
                let color = auth_hover_color(base, hover, amount);
                button
                    .custom(variant.color(color).hover(color))
                    .bg(color)
                    .opacity(0.96 + amount * 0.04)
            },
        )
        .into_any_element()
}

// ponytail: static display only — no sync status is wired from the `sync`
// crate into the GUI yet, so this always reads "Synced" regardless of the
// vault's real sync/pairing state. Add real wiring if that's ever needed.
pub(crate) fn sync_status_pill(theme: Theme) -> AnyElement {
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
}

/// Best-effort display label for a recent item's secondary line: the login's
/// site host if it has one, or "Secure note" / a bare "Login" fallback.
pub(crate) fn recent_item_subtitle(payload: &nox_core::ItemPayload) -> String {
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

pub(crate) fn home_stat_tile(theme: Theme, label: &'static str, value: usize) -> AnyElement {
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
pub(crate) fn home_quick_action(
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
pub(crate) const SECURE_NOTE_PAPER: Rgba = Rgba {
    r: 0.969,
    g: 0.973,
    b: 0.980,
    a: 1.,
};

/// Everything that only has meaning while a vault is unlocked: the vault
/// handle itself plus the view-scoped UI state that lives alongside it.
pub(crate) struct VaultSession {
    pub(crate) vault: Vault,
    pub(crate) list: VaultListState,
    pub(crate) item_editor: Option<ItemEditorState>,
    pub(crate) active_view: ActiveView,
    /// The deleted item previewed in the Trash view, if any.
    pub(crate) trash_selected: Option<nox_core::ItemId>,
    /// Whether the selected item's password is shown in plaintext in the detail panel.
    pub(crate) reveal_password: bool,
}

/// The top-level vault lifecycle state.
#[allow(clippy::large_enum_variant)]
pub enum AppState {
    NoVault,
    RegistryError,
    Locked,
    Unlocked(VaultSession),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum FormState {
    Idle,
    Pending,
    Error(SharedString),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemoveVaultDialogState {
    pub(crate) index: usize,
    pub(crate) name: SharedString,
    pub(crate) file_exists: bool,
    pub(crate) delete_files: bool,
}

#[derive(Clone)]
pub(crate) struct RenameVaultDialogState {
    pub(crate) index: usize,
    pub(crate) original: SharedString,
    pub(crate) input: Entity<InputState>,
    /// Set when the user confirms an empty name: the dialog stays open and
    /// says why, rather than closing on a name that was never applied.
    pub(crate) error: bool,
}

#[derive(Clone)]
pub(crate) struct ChangePasswordDialogState {
    pub(crate) current: Entity<InputState>,
    pub(crate) new_password: Entity<InputState>,
    pub(crate) confirm: Entity<InputState>,
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

    pub(crate) create_name: Entity<InputState>,
    pub(crate) create_password: Entity<InputState>,
    pub(crate) create_confirm: Entity<InputState>,
    pub(crate) create_state: FormState,
    _create_task: Task<()>,

    pub(crate) unlock_password: Entity<InputState>,
    pub(crate) unlock_state: FormState,
    _unlock_task: Task<()>,

    inactivity_timeout: Duration,
    last_activity: Instant,
    inactivity_epoch: usize,
    _inactivity_task: Task<()>,

    /// Freshly rendered (title, body) for the open item-editor Sheet, refreshed
    /// every `render_unlocked` pass. See `open_item_editor_sheet` for why this
    /// indirection exists instead of the Sheet reading `Nox` directly.
    pub(crate) item_editor_sheet_cell: Rc<RefCell<Option<(SharedString, AnyElement)>>>,
    pub(crate) clipboard: ClipboardState,
    pub(crate) conflicts: ConflictState,
    pub(crate) conflicts_open: bool,
    pub(crate) backup: BackupState,
    pub(crate) window_controls: Entity<WindowControls>,
    pub(crate) settings: Settings,
    pub(crate) settings_section: SettingsSection,
    pub(crate) settings_search: Entity<InputState>,
    pub(crate) settings_auto_lock_select: Entity<SelectState<SettingsDurationDelegate>>,
    pub(crate) _settings_auto_lock_subscription: Subscription,
    pub(crate) settings_clipboard_select: Entity<SelectState<SettingsDurationDelegate>>,
    pub(crate) _settings_clipboard_subscription: Subscription,
    pub(crate) settings_open: bool,
    pub(crate) remove_vault_dialog: Option<RemoveVaultDialogState>,
    pub(crate) rename_vault_dialog: Option<RenameVaultDialogState>,
    pub(crate) rename_vault_dialog_focus: FocusHandle,
    pub(crate) rename_vault_cancel_focus: FocusHandle,
    pub(crate) rename_vault_confirm_focus: FocusHandle,
    pub(crate) rename_vault_prior_focus: Option<FocusHandle>,
    pub(crate) remove_vault_dialog_focus: FocusHandle,
    pub(crate) remove_vault_option_focus: FocusHandle,
    pub(crate) remove_vault_cancel_focus: FocusHandle,
    pub(crate) remove_vault_confirm_focus: FocusHandle,
    pub(crate) remove_vault_prior_focus: Option<FocusHandle>,
    pub(crate) change_password_dialog: Option<ChangePasswordDialogState>,
    pub(crate) change_password_state: FormState,
    pub(crate) _change_password_task: Task<()>,
    pub(crate) change_password_dialog_focus: FocusHandle,
    pub(crate) change_password_cancel_focus: FocusHandle,
    pub(crate) change_password_confirm_focus: FocusHandle,
    pub(crate) change_password_prior_focus: Option<FocusHandle>,
    /// Hover state per animated button id. Keyed by `String` rather than
    /// `&'static str` so per-row buttons (the Trash list) can take part too.
    pub(crate) auth_hovered: HashMap<String, bool>,
    /// Revealed locked copy blocks in the read-only secure note detail panel.
    pub(crate) detail_revealed_copy_blocks: BTreeSet<(nox_core::ItemId, usize)>,
    /// Subscriptions for the website-blur listener on the current editor.
    pub(crate) editor_blur_subscriptions: Vec<Subscription>,
    /// Locally cached favicon/upload selections, keyed by item key (or, for
    /// an editor still being created, its editor key). Loaded once at
    /// startup from the private icon cache and re-persisted on every
    /// change, so list/detail rendering and a reopened editor both see the
    /// same selection without any bytes ever entering synced item state.
    pub(crate) local_icon_selections: HashMap<String, crate::icons::LocalIconRef>,
    /// Standalone password generator opened from the Home quick action, used
    /// without an item editor open. Mirrors the item-editor generator state.
    pub(crate) password_generator: GeneratorPopoverState,
    pub(crate) password_generator_copied: bool,
    /// Bumped on every `copy_generated_password` call so a stale revert task
    /// from an earlier copy can't clear a flag a newer copy just set — the
    /// same epoch-guard pattern `ClipboardState` uses for its own timer.
    pub(crate) password_generator_copy_epoch: u64,
    pub(crate) _password_generator_copy_task: Task<()>,
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
        let settings_search = Self::new_input(window, cx, "Search settings…", false);
        let locker = cx.weak_entity();
        let settings = load_settings(&data_dir);
        let settings_auto_lock_select = cx.new(|cx| {
            SelectState::new(
                SettingsDurationDelegate::new(settings::AUTO_LOCK_DURATIONS),
                settings::settings_duration_index(
                    settings::AUTO_LOCK_DURATIONS,
                    settings.auto_lock_seconds,
                ),
                window,
                cx,
            )
        });
        let _settings_auto_lock_subscription = cx.subscribe_in(
            &settings_auto_lock_select,
            window,
            |this: &mut Self,
             _select,
             event: &SelectEvent<SettingsDurationDelegate>,
             window,
             cx| {
                let SelectEvent::Confirm(value) = event;
                if let Some(seconds) = value {
                    let mut settings = this.settings.clone();
                    settings.auto_lock_seconds = *seconds;
                    this.update_settings(settings, window, cx);
                }
            },
        );
        let settings_clipboard_select = cx.new(|cx| {
            SelectState::new(
                SettingsDurationDelegate::new(settings::CLIPBOARD_DURATIONS),
                settings::settings_duration_index(
                    settings::CLIPBOARD_DURATIONS,
                    settings.clipboard_seconds,
                ),
                window,
                cx,
            )
        });
        let _settings_clipboard_subscription = cx.subscribe_in(
            &settings_clipboard_select,
            window,
            |this: &mut Self,
             _select,
             event: &SelectEvent<SettingsDurationDelegate>,
             window,
             cx| {
                let SelectEvent::Confirm(value) = event;
                if let Some(seconds) = value {
                    let mut settings = this.settings.clone();
                    settings.clipboard_seconds = *seconds;
                    this.update_settings(settings, window, cx);
                }
            },
        );
        let local_icon_selections = crate::icons::load_local_selections(&data_dir);
        let password_generator = Self::fresh_password_generator(window, cx);
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
            item_editor_sheet_cell: Rc::new(RefCell::new(None)),
            clipboard: ClipboardState::new(clipboard_timeout),
            conflicts: ConflictState::Closed,
            conflicts_open: false,
            backup: BackupState::new(),
            window_controls,
            settings,
            settings_section: SettingsSection::Appearance,
            settings_search,
            settings_auto_lock_select,
            _settings_auto_lock_subscription,
            settings_clipboard_select,
            _settings_clipboard_subscription,
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
            change_password_dialog: None,
            change_password_state: FormState::Idle,
            _change_password_task: Task::ready(()),
            change_password_dialog_focus: cx.focus_handle(),
            change_password_cancel_focus: cx.focus_handle().tab_stop(true),
            change_password_confirm_focus: cx.focus_handle().tab_stop(true),
            change_password_prior_focus: None,
            auth_hovered: HashMap::new(),
            detail_revealed_copy_blocks: BTreeSet::new(),
            editor_blur_subscriptions: Vec::new(),
            local_icon_selections,
            password_generator,
            password_generator_copied: false,
            password_generator_copy_epoch: 0,
            _password_generator_copy_task: Task::ready(()),
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

    /// The active vault session, if the app is currently unlocked.
    pub(crate) fn session(&self) -> Option<&VaultSession> {
        match &self.state {
            AppState::Unlocked(session) => Some(session),
            _ => None,
        }
    }

    /// Mutable access to the active vault session, if the app is currently unlocked.
    pub(crate) fn session_mut(&mut self) -> Option<&mut VaultSession> {
        match &mut self.state {
            AppState::Unlocked(session) => Some(session),
            _ => None,
        }
    }

    /// The open item editor, if a vault is unlocked and an editor is open.
    pub(crate) fn item_editor(&self) -> Option<&ItemEditorState> {
        self.session()
            .and_then(|session| session.item_editor.as_ref())
    }

    /// Mutable access to the open item editor, if a vault is unlocked and an
    /// editor is open.
    pub(crate) fn item_editor_mut(&mut self) -> Option<&mut ItemEditorState> {
        self.session_mut()
            .and_then(|session| session.item_editor.as_mut())
    }

    /// Records one local icon selection (a favicon fetch or a manual upload)
    /// under its cache key and persists the whole map, so a reopened editor
    /// or another view's resolver can find it without re-fetching or
    /// re-uploading. The key is `LocalIconRef::item_key`, which for a
    /// still-unsaved Create editor is its editor key until `save_item`
    /// migrates it to the item's real key.
    pub(crate) fn record_local_icon_selection(&mut self, selection: crate::icons::LocalIconRef) {
        self.local_icon_selections
            .insert(selection.item_key.clone(), selection);
        if let Err(error) =
            crate::icons::persist_local_selections(&self.data_dir, &self.local_icon_selections)
        {
            eprintln!("local icon selection persistence failed: {error}");
        }
    }

    /// Removes a local icon selection (picking Default/a preset after an
    /// upload or fetch, or an explicit delete) so the resolver falls back to
    /// `IconChoice` again instead of rendering a stale local image forever.
    pub(crate) fn clear_local_icon_selection(&mut self, item_key: &str) {
        if self.local_icon_selections.remove(item_key).is_some()
            && let Err(error) =
                crate::icons::persist_local_selections(&self.data_dir, &self.local_icon_selections)
        {
            eprintln!("local icon selection persistence failed: {error}");
        }
    }

    pub(crate) fn new_input(
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
    pub(crate) fn return_to_unlock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    pub(crate) fn begin_create_from_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state = AppState::NoVault;
        self.create_state = FormState::Idle;
        self.reset_create_inputs(window, cx);
        Self::focus_input(&self.create_name, window, cx);
        cx.notify();
    }

    pub(crate) fn create_vault(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
                            let list =
                                VaultListState::from_initial_load(items, deleted, window, cx);
                            this.state = AppState::Unlocked(VaultSession {
                                vault,
                                list,
                                item_editor: None,
                                active_view: ActiveView::Home,
                                trash_selected: None,
                                reveal_password: false,
                            });
                            crate::theme::apply(ThemeMode::Dark, Some(window), cx);
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

    pub(crate) fn unlock_vault(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
                            let list =
                                VaultListState::from_initial_load(items, deleted, window, cx);
                            this.state = AppState::Unlocked(VaultSession {
                                vault,
                                list,
                                item_editor: None,
                                active_view: ActiveView::Home,
                                trash_selected: None,
                                reveal_password: false,
                            });
                            crate::theme::apply(ThemeMode::Dark, Some(window), cx);
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

    pub(crate) fn lock_vault(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(&self.state, AppState::Unlocked(_)) {
            return;
        }
        window.close_all_dialogs(cx);
        window.close_sheet(cx);
        self.discard_clipboard_state(cx);
        self.inactivity_epoch += 1;
        self._inactivity_task = Task::ready(());
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
        if let AppState::Unlocked(session) = std::mem::replace(&mut self.state, AppState::Locked) {
            session.vault.lock();
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
            WindowCommand::GeneratePassword => self.open_password_generator(window, cx),
            WindowCommand::Close => window.remove_window(),
        }
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
            let data = settings::SettingsDialogData {
                settings: self.settings.clone(),
                settings_search: self.settings_search.clone(),
                settings_query: self.settings_search.read(cx).value().to_string(),
                settings_auto_lock_select: self.settings_auto_lock_select.clone(),
                settings_clipboard_select: self.settings_clipboard_select.clone(),
                vault_list: settings::VaultListModel {
                    entries: self.vaults.vaults.clone(),
                    active_id: self.active_vault.as_ref().map(|vault| vault.id.clone()),
                },
                conflict_count: self.conflicts.count(),
            };
            settings::render_settings_modal(theme, cx.entity(), self.settings_section, &data)
        });
        let remove_vault_modal = self.render_remove_vault_dialog(cx);
        let rename_vault_modal = self.render_rename_vault_dialog(cx);
        let change_password_modal = self.render_change_password_dialog(cx);
        let password_generator_overlay = self.render_password_generator_overlay(cx);
        rsx! {
            <div
                size_full
                relative
                flex
                flex_col
                bg={theme.canvas}
                fontFamily={APP_FONT_FAMILY}
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
                {for modal in change_password_modal {
                    {modal}
                }}
                {for overlay in password_generator_overlay {
                    {overlay}
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
        BackupError, ChangeId, ITEM_SCHEMA_VERSION, IconChoice, ItemId, ItemPayload, ItemType,
        SecretBytes,
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

    /// An `Unlocked` state wrapping `vault` with an empty item list and the
    /// same session defaults `Nox::new` starts with — for tests that jump
    /// straight to Unlocked without going through `unlock_vault`/`create_vault`.
    fn unlocked_state(vault: Vault, window: &mut Window, cx: &mut Context<Nox>) -> AppState {
        AppState::Unlocked(VaultSession {
            vault,
            list: VaultListState::from_initial_load(Ok(Vec::new()), Ok(Vec::new()), window, cx),
            item_editor: None,
            active_view: ActiveView::AllItems,
            trash_selected: None,
            reveal_password: false,
        })
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
        view.update_in(cx, |nox, window, cx| {
            nox.state = unlocked_state(vault, window, cx);
        });

        view.update_in(cx, |nox, window, app| {
            nox.remove_vault(0, true, window, app)
        });

        // Deleting the file out from under an open vault without running the
        // existing teardown would leave its key material live in memory.
        assert!(view.read_with(cx, |nox, _| nox.session().is_none()));
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

    fn set_change_password_inputs(
        view: &Entity<Nox>,
        cx: &mut VisualTestContext,
        current: &str,
        new_password: &str,
        confirm: &str,
    ) {
        view.update_in(cx, |locker, window, locker_cx| {
            let dialog = locker
                .change_password_dialog
                .clone()
                .expect("change password dialog is open");
            dialog.current.update(locker_cx, |input, input_cx| {
                input.set_value(current.to_owned(), window, input_cx);
            });
            dialog.new_password.update(locker_cx, |input, input_cx| {
                input.set_value(new_password.to_owned(), window, input_cx);
            });
            dialog.confirm.update(locker_cx, |input, input_cx| {
                input.set_value(confirm.to_owned(), window, input_cx);
            });
        });
    }

    #[gpui::test]
    fn change_password_rejects_empty_current_password(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("change-password-empty-current");
        let vault = Vault::create(b"correct", &path).unwrap();
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        view.update_in(cx, |nox, window, cx| {
            nox.state = unlocked_state(vault, window, cx);
        });
        view.update_in(cx, |locker, window, locker_cx| {
            locker.begin_change_password(window, locker_cx);
        });
        set_change_password_inputs(&view, cx, "", "new-password", "new-password");
        view.update_in(cx, |locker, window, locker_cx| {
            locker.submit_change_password(window, locker_cx);
        });
        assert_eq!(
            view.read_with(cx, |locker, _| locker.change_password_state.clone()),
            FormState::Error("Enter your current password.".into())
        );
        assert!(view.read_with(cx, |locker, _| locker.change_password_dialog.is_some()));
        cleanup(&path);
    }

    #[gpui::test]
    fn change_password_rejects_empty_new_password(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("change-password-empty-new");
        let vault = Vault::create(b"correct", &path).unwrap();
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        view.update_in(cx, |nox, window, cx| {
            nox.state = unlocked_state(vault, window, cx);
        });
        view.update_in(cx, |locker, window, locker_cx| {
            locker.begin_change_password(window, locker_cx);
        });
        set_change_password_inputs(&view, cx, "correct", "", "");
        view.update_in(cx, |locker, window, locker_cx| {
            locker.submit_change_password(window, locker_cx);
        });
        assert_eq!(
            view.read_with(cx, |locker, _| locker.change_password_state.clone()),
            FormState::Error("Enter a new password.".into())
        );
        cleanup(&path);
    }

    #[gpui::test]
    fn change_password_rejects_mismatched_confirmation(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("change-password-mismatch");
        let vault = Vault::create(b"correct", &path).unwrap();
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        view.update_in(cx, |nox, window, cx| {
            nox.state = unlocked_state(vault, window, cx);
        });
        view.update_in(cx, |locker, window, locker_cx| {
            locker.begin_change_password(window, locker_cx);
        });
        set_change_password_inputs(&view, cx, "correct", "new-password", "different");
        view.update_in(cx, |locker, window, locker_cx| {
            locker.submit_change_password(window, locker_cx);
        });
        assert_eq!(
            view.read_with(cx, |locker, _| locker.change_password_state.clone()),
            FormState::Error("Passwords do not match.".into())
        );
        cleanup(&path);
    }

    #[gpui::test]
    fn change_password_rejects_short_new_password(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("change-password-short");
        let vault = Vault::create(b"correct", &path).unwrap();
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        view.update_in(cx, |nox, window, cx| {
            nox.state = unlocked_state(vault, window, cx);
        });
        view.update_in(cx, |locker, window, locker_cx| {
            locker.begin_change_password(window, locker_cx);
        });
        set_change_password_inputs(&view, cx, "correct", "short", "short");
        view.update_in(cx, |locker, window, locker_cx| {
            locker.submit_change_password(window, locker_cx);
        });
        assert_eq!(
            view.read_with(cx, |locker, _| locker.change_password_state.clone()),
            FormState::Error("Password must be at least 8 characters.".into())
        );
        cleanup(&path);
    }

    #[gpui::test]
    fn change_password_succeeds_and_closes_the_dialog(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("change-password-success");
        let vault = Vault::create(b"correct", &path).unwrap();
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        view.update_in(cx, |nox, window, cx| {
            nox.state = unlocked_state(vault, window, cx);
        });
        view.update_in(cx, |locker, window, locker_cx| {
            locker.begin_change_password(window, locker_cx);
        });
        set_change_password_inputs(&view, cx, "correct", "new-password", "new-password");
        view.update_in(cx, |locker, window, locker_cx| {
            locker.submit_change_password(window, locker_cx);
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |locker, _| locker.change_password_dialog.is_none()));
        assert_eq!(
            view.read_with(cx, |locker, _| locker.change_password_state.clone()),
            FormState::Idle
        );
        assert!(Vault::unlock(b"new-password", &path).is_ok());
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
            matches!(&locker.state, AppState::Unlocked(session) if session.vault.vault_id() == vault_id)
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
            locker.state = unlocked_state(vault, window, locker_cx);
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
            locker.state = unlocked_state(vault, window, locker_cx);
            locker.arm_inactivity_timer(window, locker_cx);
        });
        assert_eq!(view.read_with(cx, |locker, _| locker.inactivity_epoch), 1);
        cx.executor().advance_clock(Duration::from_secs(60));
        cx.run_until_parked();
        assert!(view.read_with(cx, |locker, _| matches!(&locker.state, AppState::Locked)));

        let second_path = test_path("inactivity-rearm");
        let second_vault = Vault::create(b"correct", &second_path).unwrap();
        view.update_in(cx, |locker, window, locker_cx| {
            locker.state = unlocked_state(second_vault, window, locker_cx);
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
            view.read_with(cx, |locker, _| locker.session().unwrap().active_view),
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
            locker.session().unwrap().list.search_input.clone()
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
            locker.session().unwrap().list.search_input.clone()
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
        let vault = Vault::create(b"correct", &path).unwrap();
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
            icon: IconChoice::Default,
            note_color: nox_core::NoteColor::Blue,
            note_tags: vec![],
            favorite: false,
        };
        view.update_in(cx, |locker, window, locker_cx| {
            locker.state = unlocked_state(vault, window, locker_cx);
            if let Some(session) = locker.session_mut() {
                session.list = VaultListState::from_initial_load(
                    Ok(vec![(item_id, payload.clone())]),
                    Ok(vec![nox_core::DeletedItem {
                        item_id: ItemId::new(),
                        payload,
                        deleted_at_ms: 0,
                    }]),
                    window,
                    locker_cx,
                );
            }
        });
        let (items, filtered, deleted) = view.read_with(cx, |locker, _| {
            let list = &locker.session().unwrap().list;
            (list.items.len(), list.filtered.len(), list.deleted.len())
        });
        assert_eq!((items, filtered, deleted), (1, 1, 1));
        cleanup(&path);
    }

    fn valid_test_png() -> Vec<u8> {
        vec![
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, b'I', b'H', b'D', b'R', 0,
            0, 0, 1, 0, 0, 0, 1,
        ]
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
            icon: IconChoice::Default,
            note_color: nox_core::NoteColor::Blue,
            note_tags: vec![],
            favorite: false,
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
            locker.state = AppState::Unlocked(VaultSession {
                vault,
                list: VaultListState::from_initial_load(
                    Ok(items),
                    Ok(Vec::new()),
                    window,
                    locker_cx,
                ),
                item_editor: None,
                active_view: ActiveView::AllItems,
                trash_selected: None,
                reveal_password: false,
            });
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
            let editor = locker.session().unwrap().item_editor.as_ref().unwrap();
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
        let vault = Vault::create(b"correct", &path).unwrap();
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.state = unlocked_state(vault, window, locker_cx);
            if let Some(session) = locker.session_mut() {
                session.list = VaultListState::from_initial_load(
                    Err(VaultError::ItemNotFound),
                    Ok(Vec::new()),
                    window,
                    locker_cx,
                );
            }
        });
        assert!(view.read_with(cx, |locker, _| {
            let list = &locker.session().unwrap().list;
            matches!(list.load, vault_list::ListLoadState::Failed(_)) && list.items.is_empty()
        }));
        view.update_in(cx, |locker, window, locker_cx| {
            if let Some(session) = locker.session_mut() {
                session.list = VaultListState::from_initial_load(
                    Ok(Vec::new()),
                    Err(VaultError::ItemNotFound),
                    window,
                    locker_cx,
                );
            }
        });
        assert!(view.read_with(cx, |locker, _| {
            let list = &locker.session().unwrap().list;
            matches!(list.load, vault_list::ListLoadState::Failed(_)) && list.filtered.is_empty()
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
            locker.session().unwrap().list.search_input.clone()
        });
        view.update_in(cx, |_, window, locker_cx| {
            search.update(locker_cx, |input, input_cx| input.focus(window, input_cx));
        });
        cx.simulate_keystrokes("alp");
        assert_eq!(
            view.read_with(cx, |locker, _| locker
                .session()
                .unwrap()
                .list
                .filtered
                .len()),
            1
        );
        cx.simulate_keystrokes("backspace backspace backspace");
        assert_eq!(
            view.read_with(cx, |locker, _| locker
                .session()
                .unwrap()
                .list
                .filtered
                .len()),
            2
        );
        cleanup(&path);
    }

    #[gpui::test]
    fn favorites_only_filter_shows_only_favorited_items(cx: &mut TestAppContext) {
        init(cx);
        let mut favorite = login_payload("Favorite Login", "alice");
        favorite.favorite = true;
        let payloads = [favorite, login_payload("Plain Login", "bob")];
        let (view, cx, path, ids) = unlocked_view(cx, "favorites-filter", &payloads);
        view.update_in(cx, |locker, _window, locker_cx| {
            if let Some(session) = locker.session_mut() {
                session.list.set_favorites_only(true);
            }
            let _ = locker_cx;
        });
        assert_eq!(
            view.read_with(cx, |locker, _| locker
                .session()
                .unwrap()
                .list
                .filtered
                .clone()),
            vec![0]
        );
        view.update_in(cx, |locker, _window, locker_cx| {
            if let Some(session) = locker.session_mut() {
                session.list.set_favorites_only(false);
            }
            let _ = locker_cx;
        });
        assert_eq!(
            view.read_with(cx, |locker, _| locker
                .session()
                .unwrap()
                .list
                .filtered
                .len()),
            2
        );
        let _ = ids;
        cleanup(&path);
    }

    #[gpui::test]
    fn toggle_item_favorite_flips_vault_and_list_cache(cx: &mut TestAppContext) {
        init(cx);
        let payloads = [login_payload("GitHub", "alice")];
        let (view, cx, path, ids) = unlocked_view(cx, "toggle-favorite", &payloads);
        let item_id = ids[0];

        view.update_in(cx, |locker, _window, locker_cx| {
            locker.toggle_item_favorite(item_id, locker_cx);
        });
        view.read_with(cx, |locker, _| {
            let session = locker.session().unwrap();
            assert!(session.vault.get_item(item_id).unwrap().unwrap().favorite);
            let (_, cached) = session
                .list
                .items
                .iter()
                .find(|(id, _)| *id == item_id)
                .unwrap();
            assert!(cached.favorite);
        });

        view.update_in(cx, |locker, _window, locker_cx| {
            locker.toggle_item_favorite(item_id, locker_cx);
        });
        view.read_with(cx, |locker, _| {
            let session = locker.session().unwrap();
            assert!(!session.vault.get_item(item_id).unwrap().unwrap().favorite);
            let (_, cached) = session
                .list
                .items
                .iter()
                .find(|(id, _)| *id == item_id)
                .unwrap();
            assert!(!cached.favorite);
        });
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
            icon: IconChoice::Default,
            note_color: nox_core::NoteColor::Blue,
            note_tags: vec![],
            favorite: false,
        }];
        let (view, cx, path, _) = unlocked_view(cx, "secure-note-content-search", &payloads);
        view.update_in(cx, |locker, _window, locker_cx| {
            if let Some(session) = locker.session_mut() {
                session.active_view = ActiveView::SecureNotes;
            }
            locker.recompute_vault_list_filter(locker_cx);
        });
        view.update_in(cx, |locker, _window, locker_cx| {
            locker
                .session_mut()
                .unwrap()
                .list
                .recompute_filter("amber-galaxy".into());
            locker_cx.notify();
        });
        assert_eq!(
            view.read_with(cx, |locker, _| locker
                .session()
                .unwrap()
                .list
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
            let list = &locker.session().unwrap().list;
            let (id, _) = list.items.first().unwrap();
            let persisted = match &locker.state {
                AppState::Unlocked(session) => session.vault.get_item(*id).unwrap(),
                _ => None,
            };
            (*id, persisted)
        });
        assert_eq!(
            view.read_with(cx, |locker, _| locker.session().unwrap().list.selected),
            Some(id)
        );
        assert_eq!(persisted.unwrap().title, "New");
        cleanup(&path);
    }

    #[gpui::test]
    fn home_add_item_menu_creates_the_requested_type(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "home-add-item-menu", &[]);
        // Home has no active item-type filter — plain `open_create_editor`
        // would default to Login; the "+ Add item" menu bypasses that guess
        // with an explicit choice.
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_create_editor_as(nox_core::ItemType::SecureNote, window, locker_cx);
        });
        assert_eq!(
            view.read_with(cx, |locker, _| locker
                .session()
                .unwrap()
                .item_editor
                .as_ref()
                .unwrap()
                .item_type),
            nox_core::ItemType::SecureNote
        );
        cleanup(&path);
    }

    #[gpui::test]
    fn home_add_item_menu_opens_the_dedicated_full_page_workspace(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "home-add-login", &[]);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.set_active_view(ActiveView::Home, locker_cx);
            locker.open_create_editor_as(nox_core::ItemType::Login, window, locker_cx);
        });
        assert!(view.read_with(cx, |locker, _| locker.uses_login_workspace()));
        assert!(!cx.update(|window, app| window.has_active_sheet(app)));
        cleanup(&path);

        let (view, cx, path, _) = unlocked_view(cx, "home-add-note", &[]);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.set_active_view(ActiveView::Home, locker_cx);
            locker.open_create_editor_as(nox_core::ItemType::SecureNote, window, locker_cx);
        });
        assert!(view.read_with(cx, |locker, _| locker.uses_secure_note_workspace()));
        assert!(!cx.update(|window, app| window.has_active_sheet(app)));
        cleanup(&path);
    }

    #[gpui::test]
    fn choosing_a_preset_updates_the_editors_icon(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "icon-preset", &[]);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_create_editor(window, locker_cx);
            locker.choose_item_icon(
                nox_core::IconChoice::Preset(nox_core::PresetIcon::Briefcase),
                window,
                locker_cx,
            );
        });
        assert_eq!(
            view.read_with(cx, |locker, _| locker
                .session()
                .unwrap()
                .item_editor
                .as_ref()
                .unwrap()
                .icon),
            nox_core::IconChoice::Preset(nox_core::PresetIcon::Briefcase)
        );
        cleanup(&path);
    }

    #[gpui::test]
    fn secure_note_editor_has_no_favicon_option(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "icon-note-no-favicon", &[]);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.set_active_view(ActiveView::SecureNotes, locker_cx);
            locker.open_create_editor(window, locker_cx);
        });
        assert!(view.read_with(cx, |locker, _| !locker.icon_picker_offers_favicon()));
        cleanup(&path);
    }

    /// Choosing a note color updates the editor state, and saving persists the
    /// chosen color onto the encrypted payload — `get_item` hydrates the exact
    /// variant the workspace picked.
    #[gpui::test]
    fn note_color_selection_saves_to_the_payload(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "note-color", &[]);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_create_editor_as(nox_core::ItemType::SecureNote, window, locker_cx);
            locker.choose_note_color(nox_core::NoteColor::Gold, locker_cx);
        });
        assert_eq!(
            view.read_with(cx, |locker, _| locker
                .session()
                .unwrap()
                .item_editor
                .as_ref()
                .unwrap()
                .note_color),
            nox_core::NoteColor::Gold
        );
        view.update_in(cx, |locker, window, locker_cx| {
            let editor = locker.session().unwrap().item_editor.as_ref().unwrap();
            editor.title_input.update(locker_cx, |state, input_cx| {
                state.set_value("Recovery codes".to_owned(), window, input_cx)
            });
            editor.notes_input.update(locker_cx, |state, input_cx| {
                state.set_value("amber-galaxy".to_owned(), window, input_cx)
            });
            locker.save_item(window, locker_cx);
        });
        let persisted = view.read_with(cx, |locker, _| {
            let session = locker.session().unwrap();
            let (id, _) = *session.list.items.first().unwrap();
            session.vault.get_item(id).unwrap().unwrap()
        });
        assert_eq!(persisted.note_color, nox_core::NoteColor::Gold);
        cleanup(&path);
    }

    /// Free-created tags normalize into the secure-note payload and hydrate back
    /// into the edit workspace as chips.
    #[gpui::test]
    fn note_tags_save_and_hydrate_from_the_payload(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "note-tags", &[]);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_create_editor_as(nox_core::ItemType::SecureNote, window, locker_cx);
            locker.add_note_tag(" recovery ", locker_cx);
            locker.add_note_tag("RECOVERY", locker_cx);
            locker.add_note_tag("wifi", locker_cx);
            let editor = locker.session().unwrap().item_editor.as_ref().unwrap();
            editor.title_input.update(locker_cx, |state, input_cx| {
                state.set_value("Recovery codes".to_owned(), window, input_cx)
            });
            editor.notes_input.update(locker_cx, |state, input_cx| {
                state.set_value("amber-galaxy".to_owned(), window, input_cx)
            });
            locker.save_item(window, locker_cx);
        });
        let (item_id, persisted) = view.read_with(cx, |locker, _| {
            let session = locker.session().unwrap();
            let (id, _) = *session.list.items.first().unwrap();
            (id, session.vault.get_item(id).unwrap().unwrap())
        });
        assert_eq!(persisted.note_tags, ["recovery", "wifi"]);

        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_editor_for_item(item_id, window, locker_cx);
        });
        assert_eq!(
            view.read_with(cx, |locker, _| locker
                .session()
                .unwrap()
                .item_editor
                .as_ref()
                .unwrap()
                .note_tags
                .clone()),
            ["recovery", "wifi"]
        );
        cleanup(&path);
    }

    /// The editor sheet has no favorite control, but it still mirrors the
    /// loaded item's favorite bit into editor-local state so a save never
    /// silently un-favorites it.
    #[gpui::test]
    fn editing_preserves_favorite_without_a_favorite_control(cx: &mut TestAppContext) {
        init(cx);
        let mut favorite = login_payload("GitHub", "alice");
        favorite.favorite = true;
        let (view, cx, path, ids) = unlocked_view(cx, "favorite-edit-preserve", &[favorite]);
        let item_id = ids[0];

        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_editor_for_item(item_id, window, locker_cx);
            locker.save_item(window, locker_cx);
        });

        let persisted = view.read_with(cx, |locker, _| {
            locker
                .session()
                .unwrap()
                .vault
                .get_item(item_id)
                .unwrap()
                .unwrap()
        });
        assert!(persisted.favorite);
        cleanup(&path);
    }

    /// Tags are removable chips and saved secure-note tags are offered as
    /// selectable suggestions: adding two tags and removing one leaves only
    /// the other, and selecting a suggestion from another secure note adds
    /// it unless it is already present. Login payloads never contribute
    /// suggestions — tags are secure-note-only.
    #[gpui::test]
    fn note_tags_remove_and_suggestions_track_editor_state(cx: &mut TestAppContext) {
        init(cx);
        let payloads = [
            ItemPayload {
                schema_version: ITEM_SCHEMA_VERSION,
                item_type: ItemType::SecureNote,
                title: "Work Wi-Fi".into(),
                username: String::new(),
                password: String::new(),
                uris: Vec::new(),
                notes: String::new(),
                created_at: 1,
                updated_at: 1,
                icon: IconChoice::Default,
                note_color: nox_core::NoteColor::Blue,
                note_tags: vec!["wifi".into(), " shared ".into()],
                favorite: false,
            },
            ItemPayload {
                schema_version: ITEM_SCHEMA_VERSION,
                item_type: ItemType::SecureNote,
                title: "Router".into(),
                username: String::new(),
                password: String::new(),
                uris: Vec::new(),
                notes: String::new(),
                created_at: 2,
                updated_at: 2,
                icon: IconChoice::Default,
                note_color: nox_core::NoteColor::Blue,
                note_tags: vec!["wifi".into()],
                favorite: false,
            },
            {
                // A login whose payload happens to carry a tag-shaped string
                // must never surface it as a suggestion.
                let mut login = login_payload("GitHub", "alice");
                login.note_tags = vec!["login-only".into()];
                login
            },
        ];
        let (view, cx, path, _) = unlocked_view(cx, "note-tags-remove", &payloads);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.set_active_view(ActiveView::SecureNotes, locker_cx);
            locker.open_create_editor_as(ItemType::SecureNote, window, locker_cx);

            locker.add_note_tag("recovery", locker_cx);
            locker.add_note_tag("bank", locker_cx);
            locker.remove_note_tag("RECOVERY", locker_cx);
            assert_eq!(locker.item_editor().unwrap().note_tags, ["bank"]);

            // Suggestions come from other secure notes only, normalized and
            // deduped, exclude tags already on the editor, and default to the
            // top entries by usage/recency.
            assert_eq!(locker.note_tag_suggestions(), ["wifi", "shared"]);

            assert_eq!(locker.note_tag_suggestions_for_query("sha"), ["shared"]);

            locker.select_note_tag_suggestion("wifi", locker_cx);
            assert_eq!(locker.item_editor().unwrap().note_tags, ["bank", "wifi"]);
            assert_eq!(locker.note_tag_suggestions(), ["shared"]);

            // A suggestion that is already a chip (or a case variant of one)
            // is not added twice.
            locker.select_note_tag_suggestion("WiFi", locker_cx);
            assert_eq!(locker.item_editor().unwrap().note_tags, ["bank", "wifi"]);
        });
        cleanup(&path);
    }

    /// The workspace renders suggestions as clickable chips and each tag
    /// chip's remove affordance as a real clickable button.
    #[gpui::test]
    fn note_tags_suggestions_and_chip_remove_work_via_clicks(cx: &mut TestAppContext) {
        init(cx);
        let payloads = [
            ItemPayload {
                schema_version: ITEM_SCHEMA_VERSION,
                item_type: ItemType::SecureNote,
                title: "Wallet".into(),
                username: String::new(),
                password: String::new(),
                uris: Vec::new(),
                notes: String::new(),
                created_at: 1,
                updated_at: 1,
                icon: IconChoice::Default,
                note_color: nox_core::NoteColor::Blue,
                note_tags: vec!["bank".into()],
                favorite: false,
            },
            ItemPayload {
                schema_version: ITEM_SCHEMA_VERSION,
                item_type: ItemType::SecureNote,
                title: "Office door".into(),
                username: String::new(),
                password: String::new(),
                uris: Vec::new(),
                notes: String::new(),
                created_at: 1,
                updated_at: 1,
                icon: IconChoice::Default,
                note_color: nox_core::NoteColor::Blue,
                note_tags: vec![" work wifi ".into()],
                favorite: false,
            },
        ];
        let (view, cx, path, ids) = unlocked_view(cx, "note-tags-clicks", &payloads);
        cx.simulate_resize(gpui::size(px(1600.), px(1000.)));
        view.update_in(cx, |locker, window, locker_cx| {
            locker.set_active_view(ActiveView::SecureNotes, locker_cx);
            locker.open_editor_for_item(ids[0], window, locker_cx);
        });
        cx.run_until_parked();

        view.update_in(cx, |locker, window, locker_cx| {
            let input = locker.item_editor().unwrap().note_tag_input.clone();
            input.update(locker_cx, |state, input_cx| {
                state.set_value("wo".to_owned(), window, input_cx)
            });
        });
        cx.run_until_parked();
        let typed_option = cx
            .debug_bounds("note-tag-filter-option-work-wifi")
            .expect("typing must open a filtered combobox list");
        cx.simulate_click(typed_option.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |locker, _| locker
                .session()
                .unwrap()
                .item_editor
                .as_ref()
                .unwrap()
                .note_tags
                .clone()),
            ["bank", "work wifi"]
        );

        // Once selected, the tag is a chip — it is no longer a suggestion.
        assert!(
            cx.debug_bounds("note-tag-filter-option-work-wifi")
                .is_none()
        );
        assert!(cx.debug_bounds("note-tag-suggestion-work-wifi").is_none());

        view.update_in(cx, |locker, _window, locker_cx| {
            locker.remove_note_tag("work wifi", locker_cx);
        });
        cx.run_until_parked();

        // The other note's tag is still rendered as a fixed suggestion chip when
        // the input is empty; this bottom suggestion list is not the typed list.
        let suggestion = cx
            .debug_bounds("note-tag-suggestion-work-wifi")
            .expect("the saved tag must render as a suggestion chip");
        cx.simulate_click(suggestion.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |locker, _| locker
                .session()
                .unwrap()
                .item_editor
                .as_ref()
                .unwrap()
                .note_tags
                .clone()),
            ["bank", "work wifi"]
        );
        // Once selected, the tag is a chip — it is no longer a suggestion.
        assert!(cx.debug_bounds("note-tag-suggestion-work-wifi").is_none());

        // The first chip's remove button removes that tag.
        let remove = cx
            .debug_bounds("note-tag-remove-0")
            .expect("each chip must render a remove button");
        cx.simulate_click(remove.center(), gpui::Modifiers::none());
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |locker, _| locker
                .session()
                .unwrap()
                .item_editor
                .as_ref()
                .unwrap()
                .note_tags
                .clone()),
            ["work wifi"]
        );
        cleanup(&path);
    }

    /// `locker.pen` `S38lsY`: websites are repeatable rows. Adding appends an
    /// empty one, every non-empty row is saved, and removing the last row
    /// clears it instead of leaving the field with nothing to type into.
    #[gpui::test]
    fn website_rows_add_collect_and_never_drop_the_last_row(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "website-rows", &[]);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_create_editor(window, locker_cx);
            assert_eq!(locker.item_editor().unwrap().uri_inputs.len(), 1);

            locker.add_website_row(window, locker_cx);
            let editor = locker.item_editor().unwrap();
            assert_eq!(editor.uri_inputs.len(), 2);
            let (first, second) = (editor.uri_inputs[0].clone(), editor.uri_inputs[1].clone());
            first.update(locker_cx, |state, input_cx| {
                state.set_value("https://one.test".to_owned(), window, input_cx)
            });
            second.update(locker_cx, |state, input_cx| {
                state.set_value("https://two.test".to_owned(), window, input_cx)
            });
            assert_eq!(
                locker.item_editor().unwrap().uris(locker_cx),
                vec!["https://one.test", "https://two.test"]
            );

            locker.remove_website_row(0, window, locker_cx);
            assert_eq!(
                locker.item_editor().unwrap().uris(locker_cx),
                vec!["https://two.test"]
            );

            // The last row is cleared, not removed.
            locker.remove_website_row(0, window, locker_cx);
            let editor = locker.item_editor().unwrap();
            assert_eq!(editor.uri_inputs.len(), 1);
            assert!(editor.uris(locker_cx).is_empty());
        });
        cleanup(&path);
    }

    /// A row added after the editor opened must get its own blur listener —
    /// otherwise a URL typed into the second row would silently never trigger
    /// the favicon auto-fetch that the first row does.
    #[gpui::test]
    fn an_added_website_row_still_auto_fetches_on_blur(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "website-row-blur", &[]);
        let before = view.read_with(cx, |locker, _| locker.editor_blur_subscriptions.len());
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_create_editor(window, locker_cx);
        });
        let opened = view.read_with(cx, |locker, _| locker.editor_blur_subscriptions.len());
        assert_eq!(opened, before + 1, "the initial row registers one listener");
        view.update_in(cx, |locker, window, locker_cx| {
            locker.add_website_row(window, locker_cx);
        });
        assert_eq!(
            view.read_with(cx, |locker, _| locker.editor_blur_subscriptions.len()),
            opened + 1,
            "an added row registers its own listener"
        );
        cleanup(&path);
    }

    /// `locker.pen` `MuIQg`/`kaJQk`: icon selection is a two-tab popover, and
    /// nothing about it sits inline in the form any more — that inline grid is
    /// what put ~165px of cosmetics above the password field.
    #[test]
    fn icon_selection_is_a_two_tab_popover() {
        let source = include_str!("item_editor.rs");
        for marker in [
            "fn icon_picker_popover(",
            "fn preset_tab_body(",
            "fn custom_tab_body(",
            "\"Presets\"",
            "\"Custom\"",
        ] {
            assert!(source.contains(marker), "picker must define {marker}");
        }
        assert!(
            !source.contains("fn icon_preset_grid("),
            "the inline preset grid must be gone from the form"
        );
        assert!(
            !source.contains("20 icons available"),
            "the inline field's helper copy must be gone with it"
        );
    }

    /// The Custom tab reaches an image three ways, and every one of them lands
    /// in the same validated sink.
    #[test]
    fn custom_tab_offers_drop_browse_and_url() {
        let source = include_str!("item_editor.rs");
        for marker in [
            "on_drop(move |paths: &gpui::ExternalPaths",
            "choose_uploaded_item_icon(window, cx)",
            "fn apply_icon_url(",
            "Drop an image here or browse",
        ] {
            assert!(source.contains(marker), "custom tab must offer {marker}");
        }
        // A pasted URL is as untrusted as a site-derived one.
        let favicon = include_str!("favicon.rs");
        assert!(
            favicon.contains("fn fetch_image_from_url(")
                && favicon.contains("is_disallowed_favicon_target(&parsed)"),
            "URL fetch must reuse the favicon guard rails"
        );
    }

    /// Upload and URL failures used to be discarded with `let _ =`, so an
    /// oversized file did nothing at all and said nothing about it.
    #[test]
    fn icon_failures_reach_the_user() {
        let source = include_str!("item_editor.rs");
        assert!(
            source.contains("icon_error: Option<SharedString>"),
            "the editor carries an icon error"
        );
        assert!(
            !source.contains("let _ = locker.upload_item_icon_bytes("),
            "upload errors must not be swallowed"
        );
    }

    /// Clicking a swatch selects that preset, and the grid reflects whatever
    /// the editor currently holds — the selected state is the whole point of
    /// showing the presets inline instead of behind a popover.
    #[gpui::test]
    fn choosing_a_preset_updates_the_editor_icon(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "icon-grid-select", &[]);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_create_editor(window, locker_cx);
            assert_eq!(
                locker.item_editor().unwrap().icon,
                nox_core::IconChoice::Default
            );
            locker.choose_item_icon(
                nox_core::IconChoice::Preset(nox_core::PresetIcon::Globe),
                window,
                locker_cx,
            );
        });
        assert_eq!(
            view.read_with(cx, |locker, _| locker.item_editor().unwrap().icon),
            nox_core::IconChoice::Preset(nox_core::PresetIcon::Globe)
        );
        cleanup(&path);
    }

    #[test]
    fn login_websites_field_matches_design_copy() {
        let source = include_str!("item_editor.rs");
        assert!(
            source.contains("Add sign-in or app URLs"),
            "locker.pen S38lsY helper copy"
        );
        assert!(source.contains("Add website"), "locker.pen j0b4vA action");
        assert!(
            !source.contains("One per line"),
            "the newline-separated textarea copy must not remain"
        );
    }

    #[gpui::test]
    fn login_editor_offers_favicon(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "icon-login-favicon", &[]);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_create_editor(window, locker_cx);
        });
        assert!(view.read_with(cx, |locker, _| locker.icon_picker_offers_favicon()));
        cleanup(&path);
    }

    /// The picker's "Default" row previews the *item type's* own icon — a
    /// Secure Note's default is `file-lock`, not Login's `key-square`.
    #[gpui::test]
    fn website_blur_auto_fetches_only_changed_non_empty_login_values(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "icon-auto-fetch", &[]);
        let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let requests_for_client = requests.clone();
        cx.update(|_, app| {
            app.set_http_client(gpui::http_client::FakeHttpClient::create(move |_req| {
                requests_for_client.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                async move {
                    Ok(gpui::http_client::Response::builder()
                        .status(200)
                        .header("content-type", "image/png")
                        .body(valid_test_png().into())
                        .unwrap())
                }
            }));
        });
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_create_editor(window, locker_cx);
            let uris = locker.item_editor().unwrap().uri_inputs[0].clone();
            uris.update(locker_cx, |state, input_cx| {
                state.set_value("https://auto.test".to_owned(), window, input_cx)
            });
            locker.website_field_blurred(window, locker_cx);
        });
        cx.run_until_parked();
        assert_eq!(requests.load(std::sync::atomic::Ordering::Relaxed), 1);

        view.update_in(cx, |locker, window, locker_cx| {
            locker.website_field_blurred(window, locker_cx);
        });
        cx.run_until_parked();
        assert_eq!(requests.load(std::sync::atomic::Ordering::Relaxed), 1);

        view.update_in(cx, |locker, window, locker_cx| {
            let uris = locker.item_editor().unwrap().uri_inputs[0].clone();
            uris.update(locker_cx, |state, input_cx| {
                state.set_value("https://changed.test".to_owned(), window, input_cx)
            });
            locker.website_field_blurred(window, locker_cx);
        });
        cx.run_until_parked();
        assert_eq!(requests.load(std::sync::atomic::Ordering::Relaxed), 2);
        cleanup(&path);
    }

    #[gpui::test]
    fn secure_note_website_blur_never_starts_a_favicon_fetch(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "icon-note-no-auto-fetch", &[]);
        let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let requests_for_client = requests.clone();
        cx.update(|_, app| {
            app.set_http_client(gpui::http_client::FakeHttpClient::create(move |_req| {
                requests_for_client.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                async move { panic!("secure notes must not fetch favicons") }
            }));
        });
        view.update_in(cx, |locker, window, locker_cx| {
            locker.set_active_view(ActiveView::SecureNotes, locker_cx);
            locker.open_create_editor(window, locker_cx);
            let uris = locker.item_editor().unwrap().uri_inputs[0].clone();
            uris.update(locker_cx, |state, input_cx| {
                state.set_value("https://not-a-site.test".to_owned(), window, input_cx)
            });
            locker.website_field_blurred(window, locker_cx);
        });
        cx.run_until_parked();
        assert_eq!(requests.load(std::sync::atomic::Ordering::Relaxed), 0);
        cleanup(&path);
    }

    #[gpui::test]
    fn local_upload_is_not_added_to_the_encrypted_item_payload(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "icon-local-upload", &[]);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_create_editor(window, locker_cx);
            locker
                .upload_item_icon_bytes(&valid_test_png(), window, locker_cx)
                .unwrap();
        });
        let (icon, has_local, serialized) = view.read_with(cx, |locker, app| {
            let editor = locker.item_editor().unwrap();
            let payload = editor.payload_for_test(app);
            (
                editor.icon,
                editor.local_icon.is_some(),
                serde_json::to_vec(&payload).unwrap(),
            )
        });
        assert_eq!(icon, IconChoice::Default);
        assert!(has_local);
        assert!(
            !serialized
                .windows(valid_test_png().len())
                .any(|window| { window == valid_test_png().as_slice() })
        );
        cleanup(&path);
    }

    /// Regression: `choose_uploaded_item_icon` must reject an oversized file
    /// by its on-disk size (`std::fs::metadata`), not by reading the whole
    /// thing into memory first and only then discovering
    /// `validate_local_image` rejects it. Observable end state is the same
    /// either way — no icon change — which is what this proves still holds
    /// after the reorder.
    #[gpui::test]
    fn oversized_uploaded_file_is_rejected_without_a_full_read(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "icon-upload-oversized", &[]);
        let dir = test_dir("icon-upload-oversized-source");
        fs::create_dir_all(&dir).unwrap();
        let oversized_path = dir.join("too-big.png");
        fs::write(
            &oversized_path,
            vec![0u8; crate::icons::MAX_LOCAL_IMAGE_BYTES + 1],
        )
        .unwrap();

        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_create_editor(window, locker_cx);
            locker.choose_uploaded_item_icon(window, locker_cx);
        });
        let picked = oversized_path.clone();
        cx.simulate_path_prompt_response(move |_options| Some(vec![picked]));
        cx.run_until_parked();

        assert!(view.read_with(cx, |locker, _| {
            locker.item_editor().unwrap().local_icon.is_none()
        }));
        cleanup(&path);
        let _ = fs::remove_dir_all(&dir);
    }

    /// The everyday path through the real (test) file dialog still accepts a
    /// normal-sized valid image after the size-before-read reorder.
    #[gpui::test]
    fn uploaded_file_within_the_cap_is_accepted_through_the_real_dialog(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "icon-upload-via-dialog", &[]);
        let dir = test_dir("icon-upload-via-dialog-source");
        fs::create_dir_all(&dir).unwrap();
        let source_path = dir.join("icon.png");
        fs::write(&source_path, valid_test_png()).unwrap();

        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_create_editor(window, locker_cx);
            locker.choose_uploaded_item_icon(window, locker_cx);
        });
        let picked = source_path.clone();
        cx.simulate_path_prompt_response(move |_options| Some(vec![picked]));
        cx.run_until_parked();

        assert!(view.read_with(cx, |locker, _| {
            locker.item_editor().unwrap().local_icon.is_some()
        }));
        cleanup(&path);
        let _ = fs::remove_dir_all(&dir);
    }

    /// C1 regression: a successful fetch must bridge to the content-addressed
    /// local selection the editor's own resolver reads, not just the
    /// host-keyed compatibility cache.
    #[gpui::test]
    fn favicon_fetch_sets_a_local_selection_that_renders_immediately(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "icon-local-selection", &[]);
        let data_dir = path.parent().unwrap().to_path_buf();
        cx.update(|_, app| {
            app.set_http_client(gpui::http_client::FakeHttpClient::create(
                |_req| async move {
                    Ok(gpui::http_client::Response::builder()
                        .status(200)
                        .header("content-type", "image/png")
                        .body(valid_test_png().into())
                        .unwrap())
                },
            ));
        });
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_create_editor(window, locker_cx);
            let url_input = locker.item_editor().unwrap().uri_inputs[0].clone();
            url_input.update(locker_cx, |state, input_cx| {
                state.set_value("https://selection.test".to_owned(), window, input_cx)
            });
            locker.fetch_item_favicon(window, locker_cx);
        });
        cx.run_until_parked();

        let local_key = view.read_with(cx, |locker, _| {
            locker.item_editor().unwrap().local_icon_key()
        });
        assert!(view.read_with(cx, |locker, _| {
            locker.item_editor().unwrap().local_icon.is_some()
        }));
        let selection = view
            .read_with(cx, |locker, _| {
                locker.local_icon_selections.get(&local_key).cloned()
            })
            .expect("fetch success records a local selection under the editor's key");
        assert_eq!(fs::read(&selection.cache_path).unwrap(), valid_test_png());
        cleanup(&path);
        let _ = fs::remove_dir_all(&data_dir);
    }

    /// C4 regression: an uploaded image cached under a throwaway Create editor
    /// key must still be found after the item is saved and its editor reopened
    /// under the item's real key.
    #[gpui::test]
    fn uploaded_icon_survives_save_and_reopen(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "icon-upload-reopen", &[]);
        let data_dir = path.parent().unwrap().to_path_buf();
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_create_editor(window, locker_cx);
            locker
                .upload_item_icon_bytes(&valid_test_png(), window, locker_cx)
                .unwrap();
        });
        set_editor_values(&view, cx, "Uploaded", "alice", "secret");
        view.update_in(cx, |locker, window, locker_cx| {
            locker.save_item(window, locker_cx);
        });
        let item_id = view
            .read_with(cx, |locker, _| locker.session().unwrap().list.selected)
            .unwrap();

        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_editor_for_item(item_id, window, locker_cx);
        });
        let selection = view
            .read_with(cx, |locker, _| {
                locker.item_editor().unwrap().local_icon.clone()
            })
            .expect("the reopened editor finds the item's saved local selection");
        assert_eq!(selection.item_key, crate::icons::item_key(item_id));
        assert_eq!(fs::read(&selection.cache_path).unwrap(), valid_test_png());
        cleanup(&path);
        let _ = fs::remove_dir_all(&data_dir);
    }

    /// Regression: picking Default after an upload must actually replace the
    /// local image — previously `resolve_item_icon`'s local-selection lookup
    /// kept preferring the old bytes forever, making the pick a silent
    /// no-op. Covers both the in-memory editor state and a save+reopen, and
    /// doubles as the delete "x" affordance's underlying behavior, since
    /// that button calls the very same `choose_item_icon(Default, ..)`.
    #[gpui::test]
    fn picking_default_after_a_local_image_clears_it_and_falls_back(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "icon-clear-on-default", &[]);
        let data_dir = path.parent().unwrap().to_path_buf();
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_create_editor(window, locker_cx);
            locker
                .upload_item_icon_bytes(&valid_test_png(), window, locker_cx)
                .unwrap();
        });
        let local_key = view.read_with(cx, |locker, _| {
            locker.item_editor().unwrap().local_icon_key()
        });
        assert!(view.read_with(cx, |locker, _| {
            locker.local_icon_selections.contains_key(&local_key)
        }));

        view.update_in(cx, |locker, window, locker_cx| {
            locker.choose_item_icon(IconChoice::Default, window, locker_cx);
        });
        let (icon, has_local_in_editor, still_in_map) = view.read_with(cx, |locker, _| {
            let editor = locker.item_editor().unwrap();
            (
                editor.icon,
                editor.local_icon.is_some(),
                locker.local_icon_selections.contains_key(&local_key),
            )
        });
        assert_eq!(icon, IconChoice::Default);
        assert!(
            !has_local_in_editor,
            "editor must drop the cleared local image"
        );
        assert!(
            !still_in_map,
            "the persisted selection map must drop the entry too"
        );
        assert!(matches!(
            crate::icons::resolve_item_icon(
                nox_core::ItemType::Login,
                IconChoice::Default,
                &data_dir,
                &local_key,
                None,
            ),
            crate::icons::ResolvedIcon::Svg("icons/key-square.svg")
        ));
        // Not just in-memory: a fresh read of the on-disk index must agree.
        assert!(!crate::icons::load_local_selections(&data_dir).contains_key(&local_key));

        set_editor_values(&view, cx, "Cleared", "alice", "secret");
        view.update_in(cx, |locker, window, locker_cx| {
            locker.save_item(window, locker_cx);
        });
        let item_id = view
            .read_with(cx, |locker, _| locker.session().unwrap().list.selected)
            .unwrap();
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_editor_for_item(item_id, window, locker_cx);
        });
        assert!(view.read_with(cx, |locker, _| {
            locker.item_editor().unwrap().local_icon.is_none()
        }));
        cleanup(&path);
        let _ = fs::remove_dir_all(&data_dir);
    }

    /// Same regression as above, but for the "pick a preset" path and via a
    /// fetched favicon instead of an upload — both non-Favicon picks must
    /// clear a previously selected local image.
    #[gpui::test]
    fn picking_a_preset_after_a_fetched_favicon_clears_it(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "icon-clear-on-preset", &[]);
        cx.update(|_, app| {
            app.set_http_client(gpui::http_client::FakeHttpClient::create(
                |_req| async move {
                    Ok(gpui::http_client::Response::builder()
                        .status(200)
                        .header("content-type", "image/png")
                        .body(valid_test_png().into())
                        .unwrap())
                },
            ));
        });
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_create_editor(window, locker_cx);
            let url_input = locker.item_editor().unwrap().uri_inputs[0].clone();
            url_input.update(locker_cx, |state, input_cx| {
                state.set_value("https://preset-clear.test".to_owned(), window, input_cx)
            });
            locker.fetch_item_favicon(window, locker_cx);
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |locker, _| {
            locker.item_editor().unwrap().local_icon.is_some()
        }));

        view.update_in(cx, |locker, window, locker_cx| {
            locker.choose_item_icon(
                nox_core::IconChoice::Preset(nox_core::PresetIcon::Briefcase),
                window,
                locker_cx,
            );
        });
        let (icon, has_local) = view.read_with(cx, |locker, _| {
            let editor = locker.item_editor().unwrap();
            (editor.icon, editor.local_icon.is_some())
        });
        assert_eq!(
            icon,
            nox_core::IconChoice::Preset(nox_core::PresetIcon::Briefcase)
        );
        assert!(
            !has_local,
            "a preset pick must clear the fetched favicon too"
        );
        cleanup(&path);
    }

    /// C2 regression: the list/detail resolvers must look the saved item up in
    /// the persisted local selection map (as `resolved_item_icon` expects)
    /// instead of the bytes-blind `resolved_icon_path`.
    #[gpui::test]
    fn list_and_detail_resolvers_use_the_saved_local_icon_selection(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "icon-resolver-wiring", &[]);
        let data_dir = path.parent().unwrap().to_path_buf();
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_create_editor(window, locker_cx);
            locker
                .upload_item_icon_bytes(&valid_test_png(), window, locker_cx)
                .unwrap();
        });
        set_editor_values(&view, cx, "Uploaded", "alice", "secret");
        view.update_in(cx, |locker, window, locker_cx| {
            locker.save_item(window, locker_cx);
        });
        let item_id = view
            .read_with(cx, |locker, _| locker.session().unwrap().list.selected)
            .unwrap();

        let (payload, local_selection) = view.read_with(cx, |locker, _| {
            let payload = locker
                .session()
                .unwrap()
                .list
                .items
                .iter()
                .find(|(id, _)| *id == item_id)
                .map(|(_, payload)| payload.clone())
                .unwrap();
            let local_selection = locker
                .local_icon_selections
                .get(&crate::icons::item_key(item_id))
                .cloned();
            (payload, local_selection)
        });
        let local_selection =
            local_selection.expect("save persists the local selection under the item's real key");
        let resolved = crate::icons::resolved_item_icon(
            &data_dir,
            &crate::icons::item_key(item_id),
            &payload,
            Some(&local_selection),
        );
        assert!(matches!(
            resolved,
            crate::icons::ResolvedIcon::LocalImage(_)
        ));
        cleanup(&path);
        let _ = fs::remove_dir_all(&data_dir);
    }

    /// C3 regression: All Items' "New Item" uses the generic Sheet editor
    /// (`uses_login_workspace`/`uses_secure_note_workspace` are both false for
    /// a fresh Create there), which must still expose the avatar/picker before
    /// the name/title field. The test harness does not reliably lay out Sheet
    /// bounds, so this guards the render function's structure directly.
    #[gpui::test]
    fn all_items_create_sheet_exposes_the_icon_picker(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "icon-generic-sheet", &[]);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.set_active_view(ActiveView::AllItems, locker_cx);
            locker.open_create_editor(window, locker_cx);
        });
        assert!(!view.read_with(cx, |locker, _| locker.uses_login_workspace()));
        assert!(!view.read_with(cx, |locker, _| locker.uses_secure_note_workspace()));
        assert!(cx.update(|window, app| window.has_active_sheet(app)));

        let source = include_str!("item_editor.rs");
        let render_start = source
            .find("pub(crate) fn render_item_editor")
            .expect("render_item_editor exists");
        let generic_render = &source[render_start..];
        let picker = generic_render
            .find("let icon_picker = self.render_icon_picker(false, cx);")
            .expect("generic sheet builds the icon picker");
        let title = generic_render
            .find(r#".child(field_label("Title"))"#)
            .expect("generic sheet renders the title field");
        assert!(
            picker < title,
            "generic sheet icon picker must be before Title"
        );
        cleanup(&path);
    }

    /// I2 regression: the website-blur subscription registered for one editor
    /// opening must not still be live once that editor has been saved.
    #[gpui::test]
    fn icon_editor_save_clears_the_blur_subscriptions(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "icon-save-clears-subscriptions", &[]);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_create_editor(window, locker_cx);
        });
        assert!(!view.read_with(cx, |locker, _| locker.editor_blur_subscriptions.is_empty()));
        set_editor_values(&view, cx, "Sub", "alice", "secret");
        view.update_in(cx, |locker, window, locker_cx| {
            locker.save_item(window, locker_cx);
        });
        assert!(view.read_with(cx, |locker, _| locker.editor_blur_subscriptions.is_empty()));
        cleanup(&path);
    }

    #[gpui::test]
    fn editor_exposes_avatar_before_name_for_login_and_secure_note(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "icon-avatar-order", &[]);
        for item_type in [ItemType::Login, ItemType::SecureNote] {
            view.update_in(cx, |locker, window, locker_cx| {
                locker.open_create_editor_as(item_type, window, locker_cx);
            });
            assert!(view.read_with(cx, |locker, _| { locker.editor_avatar_is_before_name() }));
            view.update_in(cx, |locker, window, locker_cx| {
                locker.cancel_item_editor(window, locker_cx);
            });
        }
        cleanup(&path);
    }

    #[test]
    fn login_workspace_matches_avatar_design_before_name() {
        let source = include_str!("item_editor.rs");
        let render_start = source
            .find("pub(crate) fn render_login_workspace")
            .expect("login workspace renderer exists");
        let login_render = &source[render_start..];
        let avatar_label = login_render
            .find("login-avatar-row")
            .expect("login workspace renders the icon wrapper section");
        let avatar_helper = login_render
            .find("Fetched automatically from website")
            .expect("login icon wrapper explains favicon behavior");
        let avatar_picker = login_render
            .find("let icon_picker = self.render_icon_picker(true, cx);")
            .expect("login workspace builds the icon picker in avatar mode");
        let name = login_render
            .find("Login name")
            .expect("login workspace renders the name input");
        assert!(
            avatar_label < name,
            "icon wrapper section must render before name"
        );
        assert!(avatar_helper < name, "icon helper must render before name");
        assert!(avatar_picker < name, "icon picker must render before name");
        let name_input = login_render
            .find("Input::new(&title).aria_label(\"Login name\").prefix(")
            .expect("login name input uses a leading icon prefix");
        let name_icon = login_render[name_input..]
            .find("icons/key-round.svg")
            .expect("login name input uses the key fallback icon");
        assert!(
            name_input + name_icon < name + 400,
            "key fallback icon must be part of the name input"
        );
    }

    /// `locker.pen` `VQgVd`: a failed favicon fetch repaints the avatar box
    /// itself and offers the upload inline, instead of hiding both behind the
    /// popover.
    #[test]
    fn failed_favicon_fetch_matches_the_upload_design() {
        let source = include_str!("item_editor.rs");
        for marker in [
            "icons/image-off.svg",
            "icons/upload.svg",
            "login-avatar-upload",
        ] {
            assert!(
                source.contains(marker),
                "failed-fetch avatar state must render {marker}"
            );
        }
    }

    #[test]
    fn icon_picker_upload_copy_matches_design() {
        let source = include_str!("item_editor.rs");
        assert!(
            source.contains("Upload from device"),
            "locker.pen ovlEl uses Upload from device copy"
        );
        assert!(
            !source.contains("Upload from file"),
            "old upload copy must not remain in the picker"
        );
    }

    #[gpui::test]
    fn the_default_row_previews_the_item_types_own_icon(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "icon-default-row", &[]);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_create_editor(window, locker_cx);
        });
        assert_eq!(
            view.read_with(cx, |locker, _| {
                crate::icons::default_type_icon(locker.item_editor().unwrap().item_type)
            }),
            "icons/key-square.svg"
        );
        cleanup(&path);

        let (view, cx, path, _) = unlocked_view(cx, "icon-default-row-note", &[]);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.set_active_view(ActiveView::SecureNotes, locker_cx);
            locker.open_create_editor(window, locker_cx);
        });
        assert_eq!(
            view.read_with(cx, |locker, _| {
                crate::icons::default_type_icon(locker.item_editor().unwrap().item_type)
            }),
            "icons/file-lock.svg"
        );
        cleanup(&path);
    }

    /// A login with no website cannot fetch a favicon: the Custom tab's
    /// favicon action is gated on there being a website to fetch from. The old
    /// inline "type a URL just for the fetch" field is gone — a URL worth
    /// fetching from is one worth saving as a website row.
    #[gpui::test]
    fn a_login_without_a_website_cannot_fetch_a_favicon(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "icon-no-uri", &[]);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_create_editor(window, locker_cx);
        });
        assert!(view.read_with(cx, |locker, app| {
            locker.item_editor().unwrap().uris(app).is_empty()
        }));

        view.update_in(cx, |locker, window, locker_cx| {
            let uris = locker.item_editor().unwrap().uri_inputs[0].clone();
            uris.update(locker_cx, |state, input_cx| {
                state.set_value("https://saved.test".to_owned(), window, input_cx)
            });
        });
        assert!(!view.read_with(cx, |locker, app| {
            locker.item_editor().unwrap().uris(app).is_empty()
        }));
        cleanup(&path);
    }

    /// The typed URL drives the fetch and nothing else: it is never appended to
    /// the item's own URI list.
    #[gpui::test]
    fn fetching_from_a_saved_website_sets_the_favicon_icon(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "icon-typed-url", &[]);
        let data_dir = path.parent().unwrap().to_path_buf();
        cx.update(|_, app| {
            app.set_http_client(gpui::http_client::FakeHttpClient::create(
                |req| async move {
                    assert_eq!(req.uri().host(), Some("typed.test"));
                    Ok(gpui::http_client::Response::builder()
                        .status(200)
                        .header("content-type", "image/png")
                        .body(valid_test_png().into())
                        .unwrap())
                },
            ));
        });
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_create_editor(window, locker_cx);
            let url_input = locker.item_editor().unwrap().uri_inputs[0].clone();
            url_input.update(locker_cx, |state, input_cx| {
                state.set_value("https://typed.test".to_owned(), window, input_cx)
            });
            locker.fetch_item_favicon(window, locker_cx);
        });
        cx.run_until_parked();

        assert_eq!(
            view.read_with(cx, |locker, _| locker.item_editor().unwrap().icon),
            IconChoice::Favicon
        );
        assert_eq!(
            view.read_with(cx, |locker, _| locker.favicon_fetch_status()),
            item_editor::FaviconFetchStatus::Idle
        );
        assert_eq!(
            fs::read(crate::favicon::favicon_cache_path(&data_dir, "typed.test")).unwrap(),
            valid_test_png()
        );
        // The website the fetch used is the item's own saved website now —
        // the old flow fetched from a URL it deliberately threw away.
        assert_eq!(
            view.read_with(cx, |locker, app| locker.item_editor().unwrap().uris(app)),
            vec!["https://typed.test"]
        );
        cleanup(&path);
        let _ = fs::remove_dir_all(&data_dir);
    }

    /// A failed fetch says so inline and leaves the chosen icon alone.
    #[gpui::test]
    fn a_failed_favicon_fetch_reports_inline_and_keeps_the_icon(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "icon-fetch-failure", &[]);
        let data_dir = path.parent().unwrap().to_path_buf();
        cx.update(|_, app| {
            app.set_http_client(gpui::http_client::FakeHttpClient::create(
                |_req| async move {
                    Ok(gpui::http_client::Response::builder()
                        .status(404)
                        .body(Vec::new().into())
                        .unwrap())
                },
            ));
        });
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_create_editor(window, locker_cx);
            let url_input = locker.item_editor().unwrap().uri_inputs[0].clone();
            url_input.update(locker_cx, |state, input_cx| {
                state.set_value("https://dead.test".to_owned(), window, input_cx)
            });
            locker.fetch_item_favicon(window, locker_cx);
        });
        cx.run_until_parked();

        assert_eq!(
            view.read_with(cx, |locker, _| locker.item_editor().unwrap().icon),
            IconChoice::Default
        );
        assert_eq!(
            view.read_with(cx, |locker, _| locker.favicon_fetch_status()),
            item_editor::FaviconFetchStatus::Failed
        );
        assert_eq!(
            view.read_with(cx, |locker, _| locker.favicon_fetch_error()),
            Some(item_editor::FAVICON_FETCH_ERROR)
        );
        cleanup(&path);
        let _ = fs::remove_dir_all(&data_dir);
    }

    /// The row reports the in-flight fetch instead of looking inert.
    #[gpui::test]
    fn a_favicon_fetch_reports_loading_until_it_finishes(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "icon-fetch-loading", &[]);
        let data_dir = path.parent().unwrap().to_path_buf();
        let (release, released) = futures::channel::oneshot::channel::<()>();
        let gate = std::sync::Arc::new(std::sync::Mutex::new(Some(released)));
        cx.update(|_, app| {
            app.set_http_client(gpui::http_client::FakeHttpClient::create(move |_req| {
                let waiter = gate.lock().unwrap().take();
                async move {
                    if let Some(waiter) = waiter {
                        let _ = waiter.await;
                    }
                    Ok(gpui::http_client::Response::builder()
                        .status(200)
                        .header("content-type", "image/png")
                        .body(valid_test_png().into())
                        .unwrap())
                }
            }));
        });
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_create_editor(window, locker_cx);
            let url_input = locker.item_editor().unwrap().uri_inputs[0].clone();
            url_input.update(locker_cx, |state, input_cx| {
                state.set_value("https://slow.test".to_owned(), window, input_cx)
            });
            locker.fetch_item_favicon(window, locker_cx);
        });
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |locker, _| locker.favicon_fetch_status()),
            item_editor::FaviconFetchStatus::Loading
        );

        let _ = release.send(());
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |locker, _| locker.favicon_fetch_status()),
            item_editor::FaviconFetchStatus::Idle
        );
        assert_eq!(
            view.read_with(cx, |locker, _| locker.item_editor().unwrap().icon),
            IconChoice::Favicon
        );
        cleanup(&path);
        let _ = fs::remove_dir_all(&data_dir);
    }

    /// A fetch can outlive the editor that asked for it. Whatever comes back
    /// belongs to *that* editor and no other: the reply must not reach into a
    /// Secure Note the user opened in the meantime — which has no favicon at
    /// all — and silently set its icon.
    #[gpui::test]
    fn a_favicon_fetch_result_does_not_apply_to_a_different_editor(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "icon-fetch-stale-editor", &[]);
        let data_dir = path.parent().unwrap().to_path_buf();
        let (release, released) = futures::channel::oneshot::channel::<()>();
        let gate = std::sync::Arc::new(std::sync::Mutex::new(Some(released)));
        cx.update(|_, app| {
            app.set_http_client(gpui::http_client::FakeHttpClient::create(move |_req| {
                let waiter = gate.lock().unwrap().take();
                async move {
                    if let Some(waiter) = waiter {
                        let _ = waiter.await;
                    }
                    Ok(gpui::http_client::Response::builder()
                        .status(200)
                        .body(vec![7u8].into())
                        .unwrap())
                }
            }));
        });

        // A Login editor asks for a favicon and the fetch parks in flight.
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_create_editor(window, locker_cx);
            let url_input = locker.item_editor().unwrap().uri_inputs[0].clone();
            url_input.update(locker_cx, |state, input_cx| {
                state.set_value("https://slow.test".to_owned(), window, input_cx)
            });
            locker.fetch_item_favicon(window, locker_cx);
        });
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |locker, _| locker.favicon_fetch_status()),
            item_editor::FaviconFetchStatus::Loading
        );

        // The user abandons it and starts a Secure Note instead.
        view.update_in(cx, |locker, window, locker_cx| {
            locker.cancel_item_editor(window, locker_cx);
            locker.set_active_view(ActiveView::SecureNotes, locker_cx);
            locker.open_create_editor(window, locker_cx);
        });
        assert_eq!(
            view.read_with(cx, |locker, _| locker.item_editor().unwrap().item_type),
            ItemType::SecureNote
        );

        // Only now does the abandoned fetch answer.
        let _ = release.send(());
        cx.run_until_parked();

        assert_eq!(
            view.read_with(cx, |locker, _| locker.item_editor().unwrap().icon),
            IconChoice::Default
        );
        // Nor may it leave the note's row showing a status it never asked for.
        assert_eq!(
            view.read_with(cx, |locker, _| locker.favicon_fetch_status()),
            item_editor::FaviconFetchStatus::Idle
        );
        cleanup(&path);
        let _ = fs::remove_dir_all(&data_dir);
    }

    // Removed with the old picker: the note it asserted (FAVICON_TYPED_URL_NOTE) existed only for the
    // inline typed-URL field, which the two-tab picker replaced.

    /// The other way an editor stops being the one that asked: `set_editor_type`
    /// flips the *same* editor to Secure Note in place, so the id still matches
    /// and only the type check can catch it.
    #[gpui::test]
    fn a_favicon_fetch_result_does_not_apply_after_switching_to_secure_note(
        cx: &mut TestAppContext,
    ) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "icon-fetch-type-flip", &[]);
        let data_dir = path.parent().unwrap().to_path_buf();
        let (release, released) = futures::channel::oneshot::channel::<()>();
        let gate = std::sync::Arc::new(std::sync::Mutex::new(Some(released)));
        cx.update(|_, app| {
            app.set_http_client(gpui::http_client::FakeHttpClient::create(move |_req| {
                let waiter = gate.lock().unwrap().take();
                async move {
                    if let Some(waiter) = waiter {
                        let _ = waiter.await;
                    }
                    Ok(gpui::http_client::Response::builder()
                        .status(200)
                        .body(vec![7u8].into())
                        .unwrap())
                }
            }));
        });

        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_create_editor(window, locker_cx);
            let url_input = locker.item_editor().unwrap().uri_inputs[0].clone();
            url_input.update(locker_cx, |state, input_cx| {
                state.set_value("https://slow.test".to_owned(), window, input_cx)
            });
            locker.fetch_item_favicon(window, locker_cx);
        });
        cx.run_until_parked();

        // Same editor, now a Secure Note — which has no favicon at all.
        view.update(cx, |locker, locker_cx| {
            locker.set_editor_type(ItemType::SecureNote, locker_cx);
        });

        let _ = release.send(());
        cx.run_until_parked();

        assert_eq!(
            view.read_with(cx, |locker, _| locker.item_editor().unwrap().icon),
            IconChoice::Default
        );
        cleanup(&path);
        let _ = fs::remove_dir_all(&data_dir);
    }

    /// The case the type check alone cannot catch: the next editor is *also* a
    /// Login, so only the requesting editor's identity distinguishes it. A reply
    /// meant for an abandoned draft must not decorate the next item the user
    /// starts writing.
    #[gpui::test]
    fn a_favicon_fetch_result_does_not_apply_to_the_next_login_editor(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "icon-fetch-next-login", &[]);
        let data_dir = path.parent().unwrap().to_path_buf();
        let (release, released) = futures::channel::oneshot::channel::<()>();
        let gate = std::sync::Arc::new(std::sync::Mutex::new(Some(released)));
        cx.update(|_, app| {
            app.set_http_client(gpui::http_client::FakeHttpClient::create(move |_req| {
                let waiter = gate.lock().unwrap().take();
                async move {
                    if let Some(waiter) = waiter {
                        let _ = waiter.await;
                    }
                    Ok(gpui::http_client::Response::builder()
                        .status(200)
                        .body(vec![7u8].into())
                        .unwrap())
                }
            }));
        });

        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_create_editor(window, locker_cx);
            let url_input = locker.item_editor().unwrap().uri_inputs[0].clone();
            url_input.update(locker_cx, |state, input_cx| {
                state.set_value("https://slow.test".to_owned(), window, input_cx)
            });
            locker.fetch_item_favicon(window, locker_cx);
        });
        cx.run_until_parked();

        // Abandon that draft and start a second, unrelated Login.
        view.update_in(cx, |locker, window, locker_cx| {
            locker.cancel_item_editor(window, locker_cx);
            locker.open_create_editor(window, locker_cx);
        });
        assert_eq!(
            view.read_with(cx, |locker, _| locker.item_editor().unwrap().item_type),
            ItemType::Login
        );

        let _ = release.send(());
        cx.run_until_parked();

        assert_eq!(
            view.read_with(cx, |locker, _| locker.item_editor().unwrap().icon),
            IconChoice::Default
        );
        cleanup(&path);
        let _ = fs::remove_dir_all(&data_dir);
    }

    /// The cache is keyed on the URI that actually answered — filing the bytes
    /// under the first *parseable* URI instead hides them from the resolver.
    #[gpui::test]
    fn the_favicon_cache_is_keyed_on_the_uri_that_answered(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "icon-cache-host", &[]);
        let data_dir = path.parent().unwrap().to_path_buf();
        cx.update(|_, app| {
            app.set_http_client(gpui::http_client::FakeHttpClient::create(
                |req| async move {
                    if req.uri().host() == Some("alive.test") && req.uri().path() == "/favicon.ico"
                    {
                        Ok(gpui::http_client::Response::builder()
                            .status(200)
                            .header("content-type", "image/png")
                            .body(valid_test_png().into())
                            .unwrap())
                    } else {
                        Ok(gpui::http_client::Response::builder()
                            .status(404)
                            .body(Vec::new().into())
                            .unwrap())
                    }
                },
            ));
        });
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_create_editor(window, locker_cx);
            // Two rows, not two lines: each website gets its own input now.
            locker.add_website_row(window, locker_cx);
            let editor = locker.item_editor().unwrap();
            let (first, second) = (editor.uri_inputs[0].clone(), editor.uri_inputs[1].clone());
            first.update(locker_cx, |state, input_cx| {
                state.set_value("https://dead.test".to_owned(), window, input_cx)
            });
            second.update(locker_cx, |state, input_cx| {
                state.set_value("https://alive.test".to_owned(), window, input_cx)
            });
            locker.fetch_item_favicon(window, locker_cx);
        });
        cx.run_until_parked();

        assert_eq!(
            view.read_with(cx, |locker, _| locker.item_editor().unwrap().icon),
            IconChoice::Favicon
        );
        assert_eq!(
            fs::read(crate::favicon::favicon_cache_path(&data_dir, "alive.test")).unwrap(),
            valid_test_png()
        );
        assert!(!crate::favicon::favicon_cache_path(&data_dir, "dead.test").is_file());
        cleanup(&path);
        let _ = fs::remove_dir_all(&data_dir);
    }

    // Removed with the old picker: "refetch" is no longer a distinct state; the Custom tab offers the
    // favicon action whenever the login has a website.

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
                .session()
                .and_then(|session| session.item_editor.as_ref())
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
            let title = locker
                .session()
                .unwrap()
                .item_editor
                .as_ref()
                .unwrap()
                .title_input
                .clone();
            title.update(locker_cx, |input, input_cx| {
                input.set_value("Recovery codes", window, input_cx)
            });
            locker.save_item(window, locker_cx);
        });
        assert_eq!(
            view.read_with(cx, |locker, _| locker
                .session()
                .and_then(|session| session.item_editor.as_ref())
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
            let editor = locker.session().unwrap().item_editor.as_ref().unwrap();
            let password_input = editor.password_input.clone();
            password_input.update(locker_cx, |state, input_cx| {
                state.set_value("secret".to_owned(), window, input_cx);
            });
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |locker, _| {
            locker.session().unwrap().item_editor.is_some()
        }));

        view.update_in(cx, |locker, window, locker_cx| {
            locker.cancel_item_editor(window, locker_cx);
        });
        assert!(view.read_with(cx, |locker, _| {
            locker.session().unwrap().item_editor.is_none()
        }));
        cleanup(&path);
    }

    #[gpui::test]
    fn login_edit_uses_the_dedicated_workspace(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, ids) = unlocked_view(
            cx,
            "login-edit-workspace",
            &[login_payload("Existing", "alex")],
        );
        let item_id = ids[0];
        view.update_in(cx, |locker, window, locker_cx| {
            locker.set_active_view(ActiveView::Logins, locker_cx);
            locker.open_editor_for_item(item_id, window, locker_cx);
        });

        assert!(view.read_with(cx, |locker, _| {
            locker.uses_login_workspace()
                && matches!(
                    locker
                        .session()
                        .and_then(|session| session.item_editor.as_ref())
                        .map(|editor| editor.mode),
                    Some(EditorMode::Edit(id)) if id == item_id
                )
        }));
        assert!(!cx.update(|window, app| window.has_active_sheet(app)));
        cleanup(&path);
    }

    /// Editing a login from the All items view reuses the full-page "Edit
    /// login" workspace instead of the generic Sheet.
    #[gpui::test]
    fn all_items_login_edit_uses_the_dedicated_workspace(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, ids) = unlocked_view(
            cx,
            "all-items-login-edit",
            &[login_payload("Existing", "alex")],
        );
        let item_id = ids[0];
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_editor_for_item(item_id, window, locker_cx);
        });
        assert!(view.read_with(cx, |locker, _| {
            locker.session().unwrap().active_view == ActiveView::AllItems
                && locker.uses_login_workspace()
                && matches!(
                    locker
                        .session()
                        .and_then(|session| session.item_editor.as_ref())
                        .map(|editor| editor.mode),
                    Some(EditorMode::Edit(id)) if id == item_id
                )
        }));
        assert!(!cx.update(|window, app| window.has_active_sheet(app)));
        cleanup(&path);
    }

    /// Editing a secure note opens the full-page note workspace both from the
    /// Secure Notes view and from All items (creation from All items keeps
    /// the generic Sheet).
    #[gpui::test]
    fn secure_note_edit_uses_the_dedicated_workspace(cx: &mut TestAppContext) {
        init(cx);
        let payload = ItemPayload {
            schema_version: ITEM_SCHEMA_VERSION,
            item_type: ItemType::SecureNote,
            title: "Recovery codes".into(),
            username: String::new(),
            password: String::new(),
            uris: Vec::new(),
            notes: "amber-galaxy".into(),
            created_at: 1,
            updated_at: 1,
            icon: IconChoice::Default,
            note_color: nox_core::NoteColor::Blue,
            note_tags: vec![],
            favorite: false,
        };
        let (view, cx, path, ids) = unlocked_view(cx, "note-edit-workspace", &[payload]);
        let item_id = ids[0];
        view.update_in(cx, |locker, window, locker_cx| {
            locker.set_active_view(ActiveView::SecureNotes, locker_cx);
            locker.open_editor_for_item(item_id, window, locker_cx);
        });
        assert!(view.read_with(cx, |locker, _| {
            locker.uses_secure_note_workspace()
                && matches!(
                    locker
                        .session()
                        .and_then(|session| session.item_editor.as_ref())
                        .map(|editor| editor.mode),
                    Some(EditorMode::Edit(id)) if id == item_id
                )
        }));
        assert!(!cx.update(|window, app| window.has_active_sheet(app)));
        view.update_in(cx, |locker, window, locker_cx| {
            locker.cancel_item_editor(window, locker_cx);
            locker.set_active_view(ActiveView::AllItems, locker_cx);
            locker.open_editor_for_item(item_id, window, locker_cx);
        });
        assert!(view.read_with(cx, |locker, _| locker.uses_secure_note_workspace()));
        assert!(!cx.update(|window, app| window.has_active_sheet(app)));
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
            let list = &locker.session().unwrap().list;
            (list.weak_login_count(), list.reused_login_count())
        });
        assert_eq!((weak, reused), (2, 2));

        view.update_in(cx, |locker, window, locker_cx| {
            locker.select_item(ids[0], locker_cx);
            if let Some(session) = locker.session_mut() {
                session
                    .list
                    .set_login_health_filter(Some(vault_list::LoginHealth::Reused));
            }
            let _ = window;
        });
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |locker, _| locker
                .session()
                .unwrap()
                .list
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
            locker.open_editor_for_item(item_id, window, locker_cx);
        });
        set_editor_values(&view, cx, "Edited", "alice", "changed");
        view.update_in(cx, |locker, window, locker_cx| {
            locker.save_item(window, locker_cx)
        });
        assert_eq!(
            view.read_with(cx, |locker, _| locker.session().unwrap().list.items[0]
                .1
                .title
                .clone()),
            "Edited"
        );
        view.update_in(cx, |locker, window, locker_cx| {
            locker.delete_item(item_id, window, locker_cx)
        });
        assert!(view.read_with(cx, |locker, _| {
            let deleted = &locker.session().unwrap().list.deleted;
            deleted.len() == 1 && deleted[0].payload.title == "Edited"
        }));
        view.update(cx, |locker, locker_cx| {
            locker.restore_item(item_id, locker_cx)
        });
        assert!(view.read_with(cx, |locker, _| {
            locker.session().unwrap().list.deleted.is_empty()
        }));
        assert!(view.read_with(cx, |locker, _| matches!(&locker.state, AppState::Unlocked(session) if session.vault.get_item(item_id).unwrap().is_some())));
        cleanup(&path);
    }

    #[gpui::test]
    fn direct_delete_updates_cache_and_closes_editor(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, ids) =
            unlocked_view(cx, "delete-item", &[login_payload("Delete", "alice")]);
        let item_id = ids[0];
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_editor_for_item(item_id, window, locker_cx)
        });
        view.update_in(cx, |locker, window, locker_cx| {
            locker.delete_item(item_id, window, locker_cx)
        });
        assert!(view.read_with(cx, |locker, _| {
            let session = locker.session().unwrap();
            let list = &session.list;
            list.items.is_empty()
                && list.filtered.is_empty()
                && list.deleted.len() == 1
                && list.deleted[0].item_id == item_id
                && session.item_editor.is_none()
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
                let list = &locker.session().unwrap().list;
                let titles: Vec<_> = list.items.iter().map(|(_, p)| p.title.clone()).collect();
                let persisted_count = match &locker.state {
                    AppState::Unlocked(session) => session.vault.list_items().unwrap().len(),
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
    fn restoring_the_previewed_item_clears_the_trash_selection(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, ids) =
            unlocked_view(cx, "trash-preview", &[login_payload("One", "one")]);
        let item_id = ids[0];
        view.update_in(cx, |locker, window, locker_cx| {
            locker.delete_item(item_id, window, locker_cx)
        });
        view.update(cx, |locker, locker_cx| {
            locker.select_trash_item(item_id, locker_cx)
        });
        assert_eq!(
            view.read_with(cx, |locker, _| locker.session().unwrap().trash_selected),
            Some(item_id)
        );
        view.update(cx, |locker, locker_cx| {
            locker.restore_item(item_id, locker_cx)
        });
        assert_eq!(
            view.read_with(cx, |locker, _| locker.session().unwrap().trash_selected),
            None
        );
        cleanup(&path);
    }

    #[gpui::test]
    fn restoring_from_trash_returns_the_item_to_the_live_list(cx: &mut TestAppContext) {
        init(cx);
        let payloads = [login_payload("One", "one"), login_payload("Two", "two")];
        let (view, cx, path, ids) = unlocked_view(cx, "trash-restore", &payloads);
        for item_id in ids.iter().copied() {
            view.update_in(cx, |locker, window, locker_cx| {
                locker.delete_item(item_id, window, locker_cx)
            });
        }
        assert!(view.read_with(cx, |locker, _| {
            let list = &locker.session().unwrap().list;
            list.items.is_empty() && list.deleted.len() == 2
        }));

        let restored = ids[0];
        view.update(cx, |locker, locker_cx| {
            locker.restore_item(restored, locker_cx)
        });

        assert!(view.read_with(cx, |locker, _| {
            let list = &locker.session().unwrap().list;
            list.deleted.len() == 1
                && list.deleted[0].item_id == ids[1]
                && list
                    .items
                    .iter()
                    .any(|(id, item)| *id == restored && item.title == "One")
        }));
        cleanup(&path);
    }

    #[gpui::test]
    fn deleting_items_refreshes_the_deleted_list_with_titles(cx: &mut TestAppContext) {
        init(cx);
        let payloads = [login_payload("One", "one"), login_payload("Two", "two")];
        let (view, cx, path, ids) = unlocked_view(cx, "deleted-titles", &payloads);
        for item_id in ids.iter().copied() {
            view.update_in(cx, |locker, window, locker_cx| {
                locker.delete_item(item_id, window, locker_cx)
            });
        }
        let titles = view.read_with(cx, |locker, _| {
            locker
                .session()
                .unwrap()
                .list
                .deleted
                .iter()
                .map(|deleted| deleted.payload.title.clone())
                .collect::<Vec<_>>()
        });
        assert_eq!(titles.len(), 2);
        assert!(titles.contains(&"One".to_string()));
        assert!(titles.contains(&"Two".to_string()));
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
    fn standalone_generator_opens_toggles_a_class_and_generates_a_password(
        cx: &mut TestAppContext,
    ) {
        init(cx);
        let path = test_path("standalone-generator-generate");
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_password_generator(window, locker_cx);
            locker.set_standalone_generator_class(nox_core::CharClasses::SYMBOLS, false, locker_cx);
            locker.generate_standalone_password(locker_cx);
        });
        let (open, generated) = view.read_with(cx, |locker, _| {
            (
                locker.password_generator.open,
                locker.password_generator.generated.clone(),
            )
        });
        assert!(open);
        let generated = generated.expect("a password should have been generated");
        assert!(!generated.as_bytes().is_empty());
        cleanup(&path);
    }

    #[gpui::test]
    fn copy_generated_password_writes_the_clipboard_and_flags_copied(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("standalone-generator-copy");
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_password_generator(window, locker_cx);
            locker.generate_standalone_password(locker_cx);
            locker.copy_generated_password(window, locker_cx);
        });
        let expected = view.read_with(cx, |locker, _| {
            locker
                .password_generator
                .generated
                .as_ref()
                .map(|password| String::from_utf8_lossy(password.as_bytes()).into_owned())
        });
        assert_eq!(clipboard_text(cx), expected);
        assert!(view.read_with(cx, |locker, _| locker.password_generator_copied));
        cx.executor()
            .advance_clock(crate::clipboard::COPY_FEEDBACK_DURATION);
        cx.run_until_parked();
        assert!(!view.read_with(cx, |locker, _| locker.password_generator_copied));
        cleanup(&path);
    }

    #[gpui::test]
    fn generate_password_window_command_opens_the_generator(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "generate-password-command", &[]);
        assert!(view.read_with(cx, |locker, _| !locker.password_generator.open));
        view.update_in(cx, |locker, window, locker_cx| {
            locker.run_window_command(WindowCommand::GeneratePassword, window, locker_cx);
        });
        assert!(view.read_with(cx, |locker, _| locker.password_generator.open));
        cleanup(&path);
    }

    #[gpui::test]
    fn newest_generated_password_copy_owns_the_copied_feedback(cx: &mut TestAppContext) {
        init(cx);
        let path = test_path("standalone-generator-copy-epoch");
        let (view, cx) = add_locker_view(cx, path.clone(), DEFAULT_INACTIVITY_TIMEOUT);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_password_generator(window, locker_cx);
            locker.generate_standalone_password(locker_cx);
            locker.copy_generated_password(window, locker_cx);
        });
        // Half the feedback window later, copy again — the first copy's
        // revert task must not clear the flag the second copy just set.
        cx.executor()
            .advance_clock(crate::clipboard::COPY_FEEDBACK_DURATION / 2);
        view.update_in(cx, |locker, window, locker_cx| {
            locker.copy_generated_password(window, locker_cx);
        });
        cx.executor()
            .advance_clock(crate::clipboard::COPY_FEEDBACK_DURATION / 2);
        cx.run_until_parked();
        assert!(
            view.read_with(cx, |locker, _| locker.password_generator_copied),
            "the first copy's timer fired but must not have cleared the second copy's feedback",
        );
        cleanup(&path);
    }

    #[gpui::test]
    fn password_generator_overlay_renders_only_when_open(cx: &mut TestAppContext) {
        init(cx);
        let (view, cx, path, _) = unlocked_view(cx, "generator-overlay-closed", &[]);
        assert!(view.update(cx, |locker, cx| {
            locker.render_password_generator_overlay(cx).is_none()
        }));
        view.update_in(cx, |locker, window, locker_cx| {
            locker.open_password_generator(window, locker_cx);
        });
        assert!(view.update(cx, |locker, cx| {
            locker.render_password_generator_overlay(cx).is_some()
        }));
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
            locker.open_editor_for_item(ids[0], window, locker_cx);
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
                locker.session().is_none(),
                locker.session().is_none(),
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
        // The lock button lives in the title bar now, before the workspace body
        // in focus order.
        for _ in 0..1 {
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
        for _ in 0..1 {
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
