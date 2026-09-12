use super::Settings;
use crate::app::{Nox, relative_opened_label};
use crate::theme::Theme;
use crate::vaults::VaultEntry;
use gpui::{
    Animation, AnimationExt, AnyElement, App, BoxShadow, Entity, FontWeight, MouseButton,
    SharedString, Window, div, ease_out_quint, point, prelude::*, px, rgb, rgba,
};
use gpui_component::{
    Icon, IndexPath, Sizable,
    button::Button,
    input::{Input, InputState},
    select::{Select, SelectDelegate, SelectItem, SelectState},
};
use std::time::Duration;

/// The registered-vault list, snapshotted in `Nox::render`.
///
/// The section renderers are pure functions over `Entity<Nox>` with no `App`
/// to read it with, so every value they need is snapshotted and handed down
/// rather than re-read from `locker`. One struct rather than two parameters.
#[derive(Clone)]
pub(crate) struct VaultListModel {
    pub(crate) entries: Vec<VaultEntry>,
    pub(crate) active_id: Option<String>,
}

/// Everything the settings dialog's three-function render chain needs beyond
/// `theme`/`locker`/`section`, bundled into one struct.
///
/// `render_settings_modal` → `render_settings_dialog` → `render_section` all
/// thread the same data down to whichever leaf section renderer ends up
/// using it; passed as separate parameters, that chain put two of these
/// three functions at clippy's argument limit.
#[derive(Clone)]
pub(crate) struct SettingsDialogData {
    pub(crate) settings: Settings,
    pub(crate) settings_search: Entity<InputState>,
    pub(crate) settings_query: String,
    pub(crate) settings_auto_lock_select: Entity<SelectState<SettingsDurationDelegate>>,
    pub(crate) settings_clipboard_select: Entity<SelectState<SettingsDurationDelegate>>,
    pub(crate) vault_list: VaultListModel,
    pub(crate) conflict_count: usize,
}

#[derive(Clone)]
pub(crate) struct SettingsDurationItem {
    seconds: u64,
}

impl SettingsDurationItem {
    pub(crate) fn new(seconds: u64) -> Self {
        Self { seconds }
    }
}

impl SelectItem for SettingsDurationItem {
    type Value = u64;

    fn title(&self) -> SharedString {
        format_duration(self.seconds).into()
    }

    fn value(&self) -> &Self::Value {
        &self.seconds
    }
}

#[derive(Clone)]
pub(crate) struct SettingsDurationDelegate(pub(crate) Vec<SettingsDurationItem>);

impl SettingsDurationDelegate {
    pub(crate) fn new(seconds: &'static [u64]) -> Self {
        Self(
            seconds
                .iter()
                .copied()
                .map(SettingsDurationItem::new)
                .collect(),
        )
    }
}

impl SelectDelegate for SettingsDurationDelegate {
    type Item = SettingsDurationItem;

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
            .position(|item| item.value() == value)
            .map(IndexPath::new)
    }
}

pub(crate) fn settings_duration_index(options: &'static [u64], selected: u64) -> Option<IndexPath> {
    options
        .iter()
        .position(|seconds| *seconds == selected)
        .map(IndexPath::new)
}

pub(crate) const AUTO_LOCK_DURATIONS: &[u64] = &[30, 60, 300, 900, 1800];
pub(crate) const CLIPBOARD_DURATIONS: &[u64] = &[5, 15, 30, 60, 300];

const MODAL_BACKDROP: u32 = 0x0A0C0F99;

pub(crate) fn render_settings_modal(
    theme: Theme,
    locker: Entity<Nox>,
    section: SettingsSection,
    data: &SettingsDialogData,
) -> AnyElement {
    let close_locker = locker.clone();
    div()
        .id("settings-modal-layer")
        .debug_selector(|| "settings-backdrop".to_owned())
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(rgba(MODAL_BACKDROP))
        .on_mouse_down(MouseButton::Left, move |_, window, app| {
            close_locker.update(app, |locker, cx| {
                locker.close_settings_dialog(window, cx);
            });
        })
        .child(
            div()
                .id("settings-dialog-shell")
                .debug_selector(|| "settings-dialog-shell".to_owned())
                .w(px(1040.))
                .h(px(800.))
                .rounded(px(12.))
                .overflow_hidden()
                .bg(theme.canvas)
                .border_1()
                .border_color(theme.border)
                .shadow(vec![BoxShadow {
                    color: rgba(0x00000066).into(),
                    offset: point(px(0.), px(24.)),
                    blur_radius: px(48.),
                    spread_radius: px(-8.),
                    inset: false,
                }])
                .on_mouse_down(MouseButton::Left, |_, _, app| app.stop_propagation())
                .child(render_settings_dialog(theme, locker, section, data)),
        )
        .with_animation(
            "settings-modal-layer",
            Animation::new(Duration::from_millis(160)).with_easing(ease_out_quint()),
            |this, delta| this.opacity(delta),
        )
        .into_any_element()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SettingsSection {
    Appearance,
    Security,
    Vault,
    Autofill,
    ImportExport,
}

impl SettingsSection {
    const ALL: [(Self, &'static str, &'static str); 5] = [
        (Self::Appearance, "Appearance", "icons/oalette.svg"),
        (Self::Security, "Security", "icons/shield-check.svg"),
        (Self::Vault, "Vault", "icons/database.svg"),
        (Self::Autofill, "Autofill", "icons/key-square.svg"),
        (
            Self::ImportExport,
            "Import & export",
            "icons/arrow-left-right.svg",
        ),
    ];

    fn index(self) -> usize {
        match self {
            Self::Appearance => 0,
            Self::Security => 1,
            Self::Vault => 2,
            Self::Autofill => 3,
            Self::ImportExport => 4,
        }
    }
}

pub(crate) fn render_settings_dialog(
    theme: Theme,
    locker: Entity<Nox>,
    section: SettingsSection,
    data: &SettingsDialogData,
) -> AnyElement {
    div()
        .id("settings-dialog")
        .relative()
        .flex()
        .size_full()
        .overflow_hidden()
        .bg(theme.canvas)
        .child(render_navigation(
            theme,
            locker.clone(),
            section,
            data.settings_search.clone(),
            data.settings_query.clone(),
        ))
        .child(
            div()
                .id("settings-content")
                .relative()
                .flex()
                .flex_col()
                .flex_1()
                .min_w(px(0.))
                .h_full()
                .p(px(34.))
                .pt(px(26.))
                .gap(px(22.))
                .bg(theme.canvas)
                .overflow_y_scroll()
                .child(render_section(theme, locker.clone(), section, data))
                .with_animation(
                    ("settings-content", section.index()),
                    Animation::new(Duration::from_millis(160)).with_easing(ease_out_quint()),
                    |this, delta| this.opacity(delta),
                ),
        )
        .child({
            let close_locker = locker.clone();
            div()
                .id("close-settings")
                .absolute()
                .top(px(14.))
                .right(px(14.))
                .size(px(30.))
                .rounded(px(7.))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(|this| this.bg(theme.raised))
                .child(
                    Icon::empty()
                        .path("icons/x.svg")
                        .size(px(15.))
                        .text_color(theme.text_muted),
                )
                .on_click(move |_, window, cx| {
                    close_locker.update(cx, |locker, cx| {
                        locker.close_settings_dialog(window, cx);
                    });
                })
        })
        .into_any_element()
}

fn render_navigation(
    theme: Theme,
    locker: Entity<Nox>,
    selected: SettingsSection,
    settings_search: Entity<InputState>,
    settings_query: String,
) -> AnyElement {
    let settings_query = settings_query.to_lowercase();
    let mut list = div().flex().flex_col().gap(px(5.));
    for (section, label, icon) in SettingsSection::ALL {
        if !settings_query.is_empty() && !label.to_lowercase().contains(&settings_query) {
            continue;
        }
        list = list.child(navigation_item(
            theme,
            locker.clone(),
            section,
            label,
            icon,
            section == selected,
        ));
    }

    div()
        .id("settings-navigation")
        .w(px(236.))
        .h_full()
        .flex()
        .flex_col()
        .gap(px(12.))
        .p(px(16.))
        .pt(px(24.))
        .flex_shrink_0()
        .bg(theme.canvas)
        .border_r_1()
        .border_color(theme.border)
        .child(
            div()
                .text_size(px(9.))
                .font_weight(FontWeight::BOLD)
                .text_color(theme.text_subtle)
                .child("SETTINGS"),
        )
        .child(
            Input::new(&settings_search)
                .prefix(
                    Icon::empty()
                        .path("icons/search.svg")
                        .size(px(15.))
                        .text_color(theme.icon_muted),
                )
                .h(px(38.))
                .w_full()
                .px(px(11.))
                .bg(theme.field)
                .border_color(theme.field_border)
                .rounded(px(7.)),
        )
        .child(list)
        .into_any_element()
}

fn navigation_item(
    theme: Theme,
    locker: Entity<Nox>,
    section: SettingsSection,
    label: &'static str,
    icon: &'static str,
    active: bool,
) -> AnyElement {
    div()
        .id(("settings-section", section.index()))
        .group(label)
        .w_full()
        .h(px(38.))
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(10.))
        .rounded(px(7.))
        .cursor_pointer()
        .bg(if active { theme.raised } else { theme.canvas })
        .when(active, |this| {
            this.border_1().border_color(theme.smart_field_border)
        })
        .hover(|this| {
            this.bg(theme.raised)
                .border_1()
                .border_color(theme.smart_field_border)
        })
        .child(
            div()
                .text_color(if active {
                    theme.text_secondary
                } else {
                    theme.text_muted
                })
                .group_hover(label, |this| this.text_color(theme.text_secondary))
                .child(Icon::empty().path(icon).size(px(16.))),
        )
        .child(
            div()
                .text_size(px(12.))
                .font_weight(FontWeight(if active { 650. } else { 500. }))
                .text_color(if active {
                    theme.text_secondary
                } else {
                    theme.text_soft
                })
                .group_hover(label, |this| this.text_color(theme.text_secondary))
                .child(label),
        )
        .on_click(move |_, _, app| {
            locker.update(app, |locker, cx| {
                locker.settings_section = section;
                cx.notify();
            });
        })
        .into_any_element()
}

fn render_section(
    theme: Theme,
    locker: Entity<Nox>,
    section: SettingsSection,
    data: &SettingsDialogData,
) -> AnyElement {
    match section {
        SettingsSection::Appearance => appearance_section(theme, locker, data.settings.clone()),
        SettingsSection::Security => security_section(
            theme,
            locker,
            data.settings_auto_lock_select.clone(),
            data.settings_clipboard_select.clone(),
            data.settings.lock_on_suspend,
            data.settings.lock_on_session_lock,
        ),
        SettingsSection::Vault => {
            vault_section(theme, locker, &data.vault_list, data.conflict_count)
        }
        SettingsSection::Autofill => {
            availability_section(theme, locker, data.settings.clone(), &AUTOFILL)
        }
        SettingsSection::ImportExport => import_export_section(theme, locker),
    }
}

fn section_intro(theme: Theme, title: &'static str, description: &'static str) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(14.))
        .child(
            div()
                .text_size(px(18.))
                .font_weight(FontWeight(600.))
                .text_color(theme.text)
                .child(title),
        )
        .child(
            div()
                .text_size(px(11.))
                .text_color(theme.text_muted)
                .child(description),
        )
        .into_any_element()
}

fn divider(theme: Theme) -> AnyElement {
    div().h(px(1.)).w_full().bg(theme.border).into_any_element()
}

fn settings_row(
    theme: Theme,
    label: &'static str,
    description: &'static str,
    control: AnyElement,
) -> AnyElement {
    settings_row_with_height(theme, 42., label, description, control)
}

fn settings_row_with_height(
    theme: Theme,
    height: f32,
    label: &'static str,
    description: &'static str,
    control: AnyElement,
) -> AnyElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .h(px(height))
        .gap(px(18.))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .gap(px(4.))
                .min_w(px(0.))
                .child(
                    div()
                        .text_size(px(12.))
                        .font_weight(FontWeight(550.))
                        .text_color(theme.text)
                        .child(label),
                )
                .child(
                    div()
                        .text_size(px(10.))
                        .text_color(theme.text_subtle)
                        .child(description),
                ),
        )
        .child(div().flex_shrink_0().child(control))
        .into_any_element()
}

fn toggle(
    theme: Theme,
    id: &'static str,
    enabled: bool,
    locker: Entity<Nox>,
    change: fn(&mut Settings),
) -> AnyElement {
    div()
        .id(id)
        .w(px(36.))
        .h(px(20.))
        .px(px(3.))
        .flex()
        .items_center()
        .rounded_full()
        .cursor_pointer()
        .hover(|this| this.opacity(0.82))
        .when(enabled, |this| this.justify_end().bg(theme.accent))
        .when(!enabled, |this| this.justify_start().bg(theme.field_border))
        .child(div().size(px(14.)).rounded_full().bg(rgb(if enabled {
            0xFFFFFF
        } else {
            0xB7C0C9
        })))
        .on_click(move |_, window, app| update_settings(locker.clone(), window, app, change))
        .into_any_element()
}

fn update_settings<F>(locker: Entity<Nox>, window: &mut Window, app: &mut App, change: F)
where
    F: FnOnce(&mut Settings),
{
    locker.update(app, |locker, cx| {
        let mut settings = locker.settings.clone();
        change(&mut settings);
        locker.update_settings(settings, window, cx);
    });
}

fn appearance_section(theme: Theme, locker: Entity<Nox>, settings: Settings) -> AnyElement {
    let theme_sync = toggle(
        theme,
        "settings-sync-system",
        settings.sync_system_theme,
        locker.clone(),
        |settings| settings.sync_system_theme = !settings.sync_system_theme,
    );

    div()
        .flex()
        .flex_col()
        .gap(px(22.))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(14.))
                .child(section_intro(
                    theme,
                    "Theme",
                    "Choose how Cipher Vault looks across your devices.",
                ))
                .child(settings_row(
                    theme,
                    "Sync with system",
                    "Follow your operating system appearance automatically.",
                    theme_sync,
                )),
        )
        .into_any_element()
}

fn security_section(
    theme: Theme,
    locker: Entity<Nox>,
    settings_auto_lock_select: Entity<SelectState<SettingsDurationDelegate>>,
    settings_clipboard_select: Entity<SelectState<SettingsDurationDelegate>>,
    lock_on_suspend: bool,
    lock_on_session_lock: bool,
) -> AnyElement {
    let auto_lock = duration_control(theme, settings_auto_lock_select);
    let clipboard = duration_control(theme, settings_clipboard_select);
    let suspend_toggle = toggle(
        theme,
        "settings-lock-on-suspend",
        lock_on_suspend,
        locker.clone(),
        |settings| settings.lock_on_suspend = !settings.lock_on_suspend,
    );
    let session_lock_toggle = toggle(
        theme,
        "settings-lock-on-session-lock",
        lock_on_session_lock,
        locker.clone(),
        |settings| settings.lock_on_session_lock = !settings.lock_on_session_lock,
    );
    div()
        .flex()
        .flex_col()
        .gap(px(22.))
        .child(section_intro(
            theme,
            "Security",
            "Keep the vault protected when you step away.",
        ))
        .child(settings_row(
            theme,
            "Lock after inactivity",
            "Nox locks automatically after this period without activity.",
            auto_lock,
        ))
        .child(divider(theme))
        .child(settings_row(
            theme,
            "Lock when the system sleeps",
            "Nox locks immediately before this computer suspends.",
            suspend_toggle,
        ))
        .child(divider(theme))
        .child(settings_row(
            theme,
            "Lock when the screen locks",
            "Nox locks when the operating system session or screen locks. Best-effort on Linux.",
            session_lock_toggle,
        ))
        .child(divider(theme))
        .child(settings_row(
            theme,
            "Clear copied secrets",
            "Secrets copied from Nox are removed from the clipboard automatically.",
            clipboard,
        ))
        .child(divider(theme))
        .child(settings_row(
            theme,
            "Change vault password",
            "Update the password used to unlock this vault.",
            action_button(
                "settings-change-password",
                "Change password",
                move |window, app| {
                    locker.update(app, |locker, cx| locker.begin_change_password(window, cx));
                },
            ),
        ))
        .into_any_element()
}

fn duration_control(
    theme: Theme,
    select: Entity<SelectState<SettingsDurationDelegate>>,
) -> AnyElement {
    Select::new(&select)
        .appearance(false)
        .menu_width(px(150.))
        .w(px(120.))
        .h(px(36.))
        .px(px(11.))
        .bg(theme.field)
        .border_1()
        .border_color(theme.field_border)
        .rounded(px(7.))
        .into_any_element()
}

fn format_duration(seconds: u64) -> String {
    match seconds {
        30 => "30 seconds".into(),
        60 => "1 minute".into(),
        value if value % 60 == 0 => format!("{} minutes", value / 60),
        value => format!("{value} seconds"),
    }
}

/// One row of the registered-vault list.
///
/// Measurements come from the `wBBRw` Pencil frame: 56 high, radius 8,
/// 12 padding and gap, a 32px avatar.
///
/// Every row rests flat and lifts to `ACTIVE`/`ACTIVE_BORDER` on hover. The
/// active vault is marked by its badge alone — giving it the lifted fill at
/// rest made every row read as already hovered, and would now be
/// indistinguishable from a row under the cursor.
fn vault_row(
    theme: Theme,
    locker: Entity<Nox>,
    index: usize,
    vault: &VaultEntry,
    active: bool,
    conflict_count: usize,
) -> AnyElement {
    let _ = conflict_count;
    let rename_locker = locker.clone();
    let missing = !vault.path.is_file();
    let group = SharedString::from(format!("settings-vault-row-{index}"));
    let name_color = if missing {
        theme.text_ghost
    } else {
        theme.text
    };
    let meta_color = if missing {
        theme.text_ghost
    } else {
        theme.text_subtle
    };
    let initials: String = vault
        .name
        .split_whitespace()
        .filter_map(|part| part.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase();
    let location = vault
        .path
        .parent()
        .and_then(|parent| parent.file_name())
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_else(|| vault.path.to_string_lossy().into_owned());
    let meta = if missing {
        format!("{location} · File not found")
    } else {
        format!(
            "{location} · {}",
            relative_opened_label(vault.last_opened_ms)
        )
    };
    let name = vault.name.clone();

    div()
        .id(("settings-vault-row", index))
        .flex()
        .items_center()
        .gap(px(12.))
        .w_full()
        .h(px(56.))
        .p(px(12.))
        .rounded(px(8.))
        .group(group.clone())
        .bg(theme.field)
        // The 1px box is always there, transparent at rest: revealing a border
        // on hover would otherwise shift every row by a pixel.
        .border_1()
        .border_color(rgba(0x00000000))
        .hover(|row| row.bg(theme.raised).border_color(theme.smart_field_border))
        .child(
            div()
                .size(px(32.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(theme.raised)
                // Without this the avatar dissolves into the hovered row,
                // which lifts to the same colour.
                .group_hover(group.clone(), |avatar| avatar.bg(theme.smart_field_border))
                .child(
                    div()
                        .text_size(px(10.))
                        .font_weight(FontWeight(700.))
                        .text_color(if missing {
                            theme.text_ghost
                        } else {
                            theme.text_soft
                        })
                        .child(initials),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .gap(px(4.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(
                            div()
                                .text_size(px(12.))
                                .font_weight(FontWeight(550.))
                                .text_color(name_color)
                                .child(name.clone()),
                        )
                        .when(active, |row| {
                            row.child(
                                div()
                                    .h(px(16.))
                                    .px(px(6.))
                                    .flex()
                                    .items_center()
                                    .rounded(px(4.))
                                    .bg(theme.raised)
                                    .border_1()
                                    .border_color(theme.smart_field_border)
                                    .child(
                                        div()
                                            .text_size(px(8.))
                                            .font_weight(FontWeight(700.))
                                            .text_color(theme.text_secondary)
                                            .child("ACTIVE"),
                                    ),
                            )
                        }),
                )
                .child(div().text_size(px(10.)).text_color(meta_color).child(meta)),
        )
        // A vault whose file is gone cannot be meaningfully renamed, so the
        // action is not offered for it — removing it is the only thing left
        // to do with that row.
        .when(!missing, |row| {
            let locker = rename_locker;
            row.child(
                div()
                    .id(("settings-vault-rename", index))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .cursor_pointer()
                    .child(
                        Icon::empty()
                            .path("icons/pencil.svg")
                            .size(px(13.))
                            .text_color(theme.text_muted),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight(550.))
                            .text_color(theme.text_soft)
                            .child("Rename"),
                    )
                    .on_click(move |_, window, app| {
                        locker.update(app, |locker, cx| {
                            locker.begin_rename_vault(index, window, cx);
                        });
                    }),
            )
        })
        .child(
            // Deliberately not `action_button`: that draws a filled, outlined
            // box, which reads as a second interactive surface stacked inside
            // an already-interactive row.
            div()
                .id(("settings-vault-remove", index))
                .flex()
                .items_center()
                .gap(px(6.))
                .cursor_pointer()
                .child(
                    Icon::empty()
                        .path("icons/circle-x.svg")
                        .size(px(13.))
                        .text_color(theme.text_muted),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .font_weight(FontWeight(550.))
                        .text_color(theme.text_soft)
                        .child("Remove"),
                )
                .on_click(move |_, window, app| {
                    locker.update(app, |locker, cx| {
                        locker.begin_remove_vault(index, window, cx);
                    });
                }),
        )
        .into_any_element()
}

fn vault_section(
    theme: Theme,
    locker: Entity<Nox>,
    vault_list: &VaultListModel,
    conflict_count: usize,
) -> AnyElement {
    let restore = locker.clone();
    let export = locker.clone();
    let rows_locker = locker.clone();
    let conflicts = locker;
    div()
        .flex()
        .flex_col()
        .gap(px(22.))
        .child(section_intro(
            theme,
            "Vault",
            "Manage the encrypted vaults stored on this device.",
        ))
        .child(div().flex().flex_col().gap(px(8.)).w_full().children(
            vault_list.entries.iter().enumerate().map(|(index, vault)| {
                vault_row(
                    theme,
                    rows_locker.clone(),
                    index,
                    vault,
                    vault_list.active_id.as_deref() == Some(vault.id.as_str()),
                    conflict_count,
                )
            }),
        ))
        .child(divider(theme))
        .child(settings_row(
            theme,
            "Backups",
            "Export a recovery copy or restore one safely.",
            div()
                .flex()
                .gap(px(8.))
                .child(action_button(
                    "settings-restore-backup",
                    "Restore",
                    move |window, app| {
                        restore.update(app, |locker, cx| locker.begin_restore(window, cx));
                    },
                ))
                .child(action_button(
                    "settings-export-backup",
                    "Export",
                    move |window, app| {
                        export.update(app, |locker, cx| locker.begin_export(window, cx));
                    },
                ))
                .into_any_element(),
        ))
        .child(divider(theme))
        .child(settings_row(
            theme,
            "Conflicts",
            "Review changes that need your decision.",
            action_button(
                "settings-conflicts",
                &format!("Review ({conflict_count})"),
                move |window, app| {
                    conflicts.update(app, |locker, cx| {
                        locker.open_conflicts(window, cx);
                        locker.close_settings_dialog(window, cx);
                    });
                },
            ),
        ))
        .into_any_element()
}

fn import_export_section(theme: Theme, locker: Entity<Nox>) -> AnyElement {
    let export = locker.clone();
    let restore = locker;
    div()
        .flex()
        .flex_col()
        .gap(px(22.))
        .child(section_intro(
            theme,
            "Import & export",
            "Move encrypted backups without exposing your vault.",
        ))
        .child(settings_row(
            theme,
            "Export encrypted backup",
            "Create a portable, password-protected recovery copy.",
            action_button(
                "settings-export-archive",
                "Export backup",
                move |window, app| {
                    export.update(app, |locker, cx| locker.begin_export(window, cx));
                },
            ),
        ))
        .child(divider(theme))
        .child(settings_row(
            theme,
            "Restore encrypted backup",
            "Replace this vault after confirming the backup password.",
            action_button(
                "settings-restore-archive",
                "Restore backup",
                move |window, app| {
                    restore.update(app, |locker, cx| locker.begin_restore(window, cx));
                },
            ),
        ))
        .into_any_element()
}

fn action_button(
    id: &'static str,
    label: &str,
    action: impl Fn(&mut Window, &mut App) + 'static,
) -> AnyElement {
    Button::new(id)
        .outline()
        .small()
        .label(label)
        .on_click(move |_, window, app| action(window, app))
        .into_any_element()
}

/// A capability this build does not have, described in one place.
///
/// The four strings and the two `fn` pointers all describe the same
/// capability, and every field is `&'static` or a function pointer — so each
/// one is a `const` rather than six positional arguments. It also makes a bug
/// class unrepresentable: `read` and `toggle` used to be loose pointers passed
/// four arguments away from the copy they belong to, so nothing stopped a
/// caller from pairing the wrong strings with the wrong toggle.
struct UnavailableCapability {
    title: &'static str,
    subtitle: &'static str,
    unavailable: &'static str,
    label: &'static str,
    read: fn(&Settings) -> bool,
    toggle: fn(&mut Settings),
}

const AUTOFILL: UnavailableCapability = UnavailableCapability {
    title: "Autofill",
    subtitle: "Fill credentials without revealing them",
    unavailable: "Autofill needs the Nox browser extension, which is not installed in this build.",
    label: "Enable autofill when the extension is available",
    read: |settings| settings.autofill_enabled,
    toggle: |settings| settings.autofill_enabled = !settings.autofill_enabled,
};

fn availability_section(
    theme: Theme,
    locker: Entity<Nox>,
    settings: Settings,
    capability: &UnavailableCapability,
) -> AnyElement {
    let &UnavailableCapability {
        title,
        subtitle,
        unavailable,
        label,
        read,
        toggle: change,
    } = capability;
    let enabled = read(&settings);
    div()
        .flex()
        .flex_col()
        .gap(px(22.))
        .child(section_intro(theme, title, subtitle))
        .child(settings_row(theme, label, unavailable, toggle(theme, "settings-availability", enabled, locker, change)))
        .child(
            div()
                .rounded(px(8.))
                .p(px(14.))
                .bg(theme.surface)
                .border_1()
                .border_color(theme.field_border)
                .text_size(px(11.))
                .text_color(theme.text_muted)
                .child("Your preference is saved locally and will be used when this capability is available."),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    #[test]
    fn settings_dialog_background_matches_app_canvas_token() {
        let source = include_str!("dialog.rs");
        let production_source = source
            .split("#[cfg(test)]")
            .next()
            .expect("production source");

        assert!(!production_source.contains("const BACKGROUND"));
        assert!(production_source.contains(".bg(theme.canvas)"));
        assert!(production_source.contains("render_navigation("));
        assert!(production_source.contains("settings_query"));
    }

    #[test]
    fn appearance_section_only_exposes_system_theme_sync() {
        let source = include_str!("dialog.rs");
        let appearance = source
            .split("fn appearance_section")
            .nth(1)
            .expect("appearance section")
            .split("fn theme_card")
            .next()
            .expect("appearance section body");

        assert!(appearance.contains("Sync with system"));
        for removed_setting in [
            "Legible bright colors",
            "Transparency",
            "Opacity",
            "Background blur",
            "Dim inactive panes",
            "Language",
            "Interface language",
        ] {
            assert!(
                !appearance.contains(removed_setting),
                "Appearance should not expose {removed_setting}"
            );
        }
    }

    #[test]
    fn privacy_section_is_removed_and_rows_keep_controls_right_aligned() {
        let source = include_str!("dialog.rs");
        let production_source = source
            .split("#[cfg(test)]")
            .next()
            .expect("production source");
        let settings_row = production_source
            .split("fn settings_row_with_height")
            .nth(1)
            .expect("settings row")
            .split("fn toggle")
            .next()
            .expect("settings row body");

        assert!(!production_source.contains("SettingsSection::Privacy"));
        assert!(!production_source.contains("fn privacy_section"));
        assert!(!production_source.contains("Privacy",));
        assert!(settings_row.contains(".flex_1()"));
        assert!(settings_row.contains(".flex_shrink_0()"));
    }

    #[test]
    fn security_duration_controls_render_selects_instead_of_cycling_on_click() {
        let source = include_str!("dialog.rs");
        let duration_control = source
            .split("fn duration_control")
            .nth(1)
            .expect("duration control")
            .split("fn format_duration")
            .next()
            .expect("duration control body");

        assert!(duration_control.contains("Select::new(&select)"));
        assert!(duration_control.contains("menu_width"));
        assert!(duration_control.contains("appearance(false)"));
        assert!(!duration_control.contains("on_click"));
        assert!(!duration_control.contains("30 => 60"));
        assert!(!duration_control.contains("60 => 300"));
    }

    #[test]
    fn settings_search_is_a_real_input_and_filters_navigation() {
        let source = include_str!("dialog.rs");
        let production_source = source
            .split("#[cfg(test)]")
            .next()
            .expect("production source");

        assert!(production_source.contains("settings_search"));
        assert!(production_source.contains("Input::new(&settings_search)"));
        assert!(production_source.contains("settings_query"));
        assert!(production_source.contains("label.to_lowercase().contains(&settings_query)"));
    }

    #[test]
    fn settings_dialog_uses_theme_tokens_for_app_chrome() {
        let source = include_str!("dialog.rs");
        let production_source = source
            .split("#[cfg(test)]")
            .next()
            .expect("production source");

        for raw_role in [
            "const SURFACE",
            "const INPUT",
            "const BORDER",
            "const INPUT_BORDER",
            "const PRIMARY",
            "const SECONDARY",
            "const MUTED",
            "const ACTIVE",
            "const ACTIVE_BORDER",
            "const ACTIVE_TEXT",
            "const ACTIVE_ICON",
            "const ACCENT",
            "const NAV_LABEL",
            "const SEARCH_BORDER",
            "const SEARCH_ICON",
            "const SEARCH_PLACEHOLDER",
        ] {
            assert!(
                !production_source.contains(raw_role),
                "{raw_role} should use Theme"
            );
        }
    }
}
