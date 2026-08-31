use super::Settings;
use crate::app::{Nox, relative_opened_label};
use crate::theme::Theme;
use crate::vaults::VaultEntry;
use gpui::{
    Animation, AnimationExt, AnyElement, App, BoxShadow, Entity, FontWeight, MouseButton,
    SharedString, Window, div, ease_out_quint, point, prelude::*, px, rgb, rgba,
};
use gpui_component::{Icon, IconName, Sizable, button::Button};
use std::time::Duration;

/// The registered-vault list, snapshotted in `Nox::render`.
///
/// The section renderers are pure functions over `Entity<Nox>` with no `App`
/// to read it with, so this follows how `Settings` and the conflict count are
/// already handed down. One struct rather than two parameters: the chain is
/// four functions deep and two of them are already at clippy's argument limit.
#[derive(Clone)]
pub(crate) struct VaultListModel {
    pub(crate) entries: Vec<VaultEntry>,
    pub(crate) active_id: Option<String>,
}

const BACKGROUND: u32 = 0x1B2029;
const SURFACE: u32 = 0x202630;
const INPUT: u32 = 0x222731;
const BORDER: u32 = 0x343B47;
const INPUT_BORDER: u32 = 0x3A4450;
const PRIMARY: u32 = 0xF3F5F7;
const SECONDARY: u32 = 0xB9C0C8;
const MUTED: u32 = 0x8E98A5;
const ACTIVE: u32 = 0x252A33;
const ACTIVE_BORDER: u32 = 0x3A414D;
const ACTIVE_TEXT: u32 = 0xD9ECF8;
const ACTIVE_ICON: u32 = 0xD9ECF8;
const ACCENT: u32 = 0x77B8DF;
const NAV_LABEL: u32 = 0x7F8996;
const SEARCH_BORDER: u32 = 0x363D4A;
const SEARCH_ICON: u32 = 0x788390;
const MODAL_BACKDROP: u32 = 0x0A0C0F99;

pub(crate) fn render_settings_modal(
    theme: Theme,
    locker: Entity<Nox>,
    settings: Settings,
    section: SettingsSection,
    vault_list: &VaultListModel,
    conflict_count: usize,
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
                .bg(rgb(BACKGROUND))
                .border_1()
                .border_color(rgb(BORDER))
                .shadow(vec![BoxShadow {
                    color: rgba(0x00000066).into(),
                    offset: point(px(0.), px(24.)),
                    blur_radius: px(48.),
                    spread_radius: px(-8.),
                    inset: false,
                }])
                .on_mouse_down(MouseButton::Left, |_, _, app| app.stop_propagation())
                .child(render_settings_dialog(
                    theme,
                    locker,
                    settings,
                    section,
                    vault_list,
                    conflict_count,
                )),
        )
        .with_animation(
            "settings-modal-layer",
            Animation::new(Duration::from_millis(160)).with_easing(ease_out_quint()),
            |this, delta| this.opacity(delta),
        )
        .into_any_element()
}
const SEARCH_PLACEHOLDER: u32 = 0x727C89;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SettingsSection {
    Appearance,
    Security,
    Vault,
    Autofill,
    Privacy,
    Notifications,
    ImportExport,
    Account,
}

impl SettingsSection {
    const ALL: [(Self, &'static str, &'static str); 8] = [
        (Self::Appearance, "Appearance", "icons/oalette.svg"),
        (Self::Security, "Security", "icons/shield-check.svg"),
        (Self::Vault, "Vault", "icons/database.svg"),
        (Self::Autofill, "Autofill", "icons/key-square.svg"),
        (Self::Privacy, "Privacy", "icons/eye-off.svg"),
        (Self::Notifications, "Notifications", "icons/bell.svg"),
        (
            Self::ImportExport,
            "Import & export",
            "icons/arrow-left-right.svg",
        ),
        (Self::Account, "Account", "icons/circle-user-around.svg"),
    ];

    fn index(self) -> usize {
        match self {
            Self::Appearance => 0,
            Self::Security => 1,
            Self::Vault => 2,
            Self::Autofill => 3,
            Self::Privacy => 4,
            Self::Notifications => 5,
            Self::ImportExport => 6,
            Self::Account => 7,
        }
    }
}

pub(crate) fn render_settings_dialog(
    theme: Theme,
    locker: Entity<Nox>,
    settings: Settings,
    section: SettingsSection,
    vault_list: &VaultListModel,
    conflict_count: usize,
) -> AnyElement {
    div()
        .id("settings-dialog")
        .relative()
        .flex()
        .size_full()
        .overflow_hidden()
        .bg(rgb(BACKGROUND))
        .child(render_navigation(locker.clone(), section))
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
                .bg(rgb(BACKGROUND))
                .overflow_y_scroll()
                .child(render_section(
                    theme,
                    locker.clone(),
                    settings,
                    section,
                    vault_list,
                    conflict_count,
                ))
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
                .hover(|this| this.bg(rgb(ACTIVE)))
                .child(
                    Icon::empty()
                        .path("icons/x.svg")
                        .size(px(15.))
                        .text_color(rgb(MUTED)),
                )
                .on_click(move |_, window, cx| {
                    close_locker.update(cx, |locker, cx| {
                        locker.close_settings_dialog(window, cx);
                    });
                })
        })
        .into_any_element()
}

fn render_navigation(locker: Entity<Nox>, selected: SettingsSection) -> AnyElement {
    let mut list = div().flex().flex_col().gap(px(5.));
    for (section, label, icon) in SettingsSection::ALL {
        list = list.child(navigation_item(
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
        .bg(rgb(BACKGROUND))
        .border_r_1()
        .border_color(rgb(BORDER))
        .child(
            div()
                .text_size(px(9.))
                .font_weight(FontWeight::BOLD)
                .text_color(rgb(NAV_LABEL))
                .child("SETTINGS"),
        )
        .child(
            div()
                .h(px(38.))
                .px(px(11.))
                .flex()
                .items_center()
                .gap(px(9.))
                .rounded(px(7.))
                .bg(rgb(INPUT))
                .border_1()
                .border_color(rgb(SEARCH_BORDER))
                .child(
                    Icon::empty()
                        .path("icons/search.svg")
                        .size(px(15.))
                        .text_color(rgb(SEARCH_ICON)),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(rgb(SEARCH_PLACEHOLDER))
                        .child("Search settings…"),
                ),
        )
        .child(list)
        .into_any_element()
}

fn navigation_item(
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
        .bg(rgb(if active { ACTIVE } else { BACKGROUND }))
        .when(active, |this| {
            this.border_1().border_color(rgb(ACTIVE_BORDER))
        })
        .hover(|this| {
            this.bg(rgb(ACTIVE))
                .border_1()
                .border_color(rgb(ACTIVE_BORDER))
        })
        .child(
            div()
                .text_color(rgb(if active { ACTIVE_ICON } else { MUTED }))
                .group_hover(label, |this| this.text_color(rgb(ACTIVE_ICON)))
                .child(Icon::empty().path(icon).size(px(16.))),
        )
        .child(
            div()
                .text_size(px(12.))
                .font_weight(FontWeight(if active { 650. } else { 500. }))
                .text_color(rgb(if active { ACTIVE_TEXT } else { SECONDARY }))
                .group_hover(label, |this| this.text_color(rgb(ACTIVE_TEXT)))
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
    settings: Settings,
    section: SettingsSection,
    vault_list: &VaultListModel,
    conflict_count: usize,
) -> AnyElement {
    match section {
        SettingsSection::Appearance => appearance_section(theme, locker, settings),
        SettingsSection::Security => security_section(theme, locker, settings),
        SettingsSection::Vault => vault_section(locker, vault_list, conflict_count),
        SettingsSection::Autofill => availability_section(locker, settings, &AUTOFILL),
        SettingsSection::Privacy => privacy_section(theme, locker, settings),
        SettingsSection::Notifications => availability_section(locker, settings, &NOTIFICATIONS),
        SettingsSection::ImportExport => import_export_section(locker),
        SettingsSection::Account => account_section(),
    }
}

fn section_intro(title: &'static str, description: &'static str) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(14.))
        .child(
            div()
                .text_size(px(18.))
                .font_weight(FontWeight(600.))
                .text_color(rgb(PRIMARY))
                .child(title),
        )
        .child(
            div()
                .text_size(px(11.))
                .text_color(rgb(MUTED))
                .child(description),
        )
        .into_any_element()
}

fn divider() -> AnyElement {
    div()
        .h(px(1.))
        .w_full()
        .bg(rgb(0x343D48))
        .into_any_element()
}

fn settings_row(label: &'static str, description: &'static str, control: AnyElement) -> AnyElement {
    settings_row_with_height(42., label, description, control)
}

fn settings_row_with_height(
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
                .gap(px(4.))
                .min_w(px(0.))
                .child(
                    div()
                        .text_size(px(12.))
                        .font_weight(FontWeight(550.))
                        .text_color(rgb(0xE1E5E9))
                        .child(label),
                )
                .child(
                    div()
                        .text_size(px(10.))
                        .text_color(rgb(NAV_LABEL))
                        .child(description),
                ),
        )
        .child(control)
        .into_any_element()
}

fn toggle(
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
        .when(enabled, |this| this.justify_end().bg(rgb(ACCENT)))
        .when(!enabled, |this| this.justify_start().bg(rgb(0x353D48)))
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
        "settings-sync-system",
        settings.sync_system_theme,
        locker.clone(),
        |settings| settings.sync_system_theme = !settings.sync_system_theme,
    );
    let bright = toggle(
        "settings-bright-colors",
        settings.bright_colors,
        locker.clone(),
        |settings| settings.bright_colors = !settings.bright_colors,
    );
    let blur = toggle(
        "settings-background-blur",
        settings.background_blur,
        locker.clone(),
        |settings| settings.background_blur = !settings.background_blur,
    );
    let dim = toggle(
        "settings-dim-inactive",
        settings.dim_inactive_panes,
        locker.clone(),
        |settings| settings.dim_inactive_panes = !settings.dim_inactive_panes,
    );
    let opacity = opacity_control(locker.clone(), settings.transparency_percent);
    let language = language_control(theme, locker, &settings.language);

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
                    "Theme",
                    "Choose how Cipher Vault looks across your devices.",
                ))
                .child(settings_row(
                    "Sync with system",
                    "Follow your operating system appearance automatically.",
                    theme_sync,
                ))
                .child(settings_row(
                    "Legible bright colors",
                    "Improve contrast for bright accent colors.",
                    bright,
                ))
                .child(theme_card()),
        )
        .child(divider())
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(13.))
                .child(
                    div()
                        .text_size(px(16.))
                        .font_weight(FontWeight(650.))
                        .text_color(rgb(PRIMARY))
                        .child("Transparency"),
                )
                .child(settings_row_with_height(
                    44.,
                    "Opacity",
                    "Adjust the background transparency of the vault.",
                    opacity,
                ))
                .child(settings_row(
                    "Background blur",
                    "Blur content behind the vault window.",
                    blur,
                ))
                .child(settings_row(
                    "Dim inactive panes",
                    "Reduce contrast in unfocused panels.",
                    dim,
                )),
        )
        .child(divider())
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(12.))
                .child(
                    div()
                        .text_size(px(16.))
                        .font_weight(FontWeight(650.))
                        .text_color(rgb(PRIMARY))
                        .child("Language"),
                )
                .child(settings_row_with_height(
                    44.,
                    "Interface language",
                    "Choose the language used throughout Cipher Vault.",
                    language,
                )),
        )
        .into_any_element()
}

fn theme_card() -> AnyElement {
    div()
        .h(px(94.))
        .p(px(12.))
        .flex()
        .items_center()
        .gap(px(16.))
        .rounded(px(8.))
        .bg(rgb(SURFACE))
        .border_1()
        .border_color(rgb(INPUT_BORDER))
        .child(
            div()
                .w(px(150.))
                .h(px(68.))
                .p(px(11.))
                .flex()
                .flex_col()
                .gap(px(7.))
                .rounded(px(6.))
                .bg(rgb(0x181D25))
                .child(div().w(px(66.)).h(px(4.)).rounded(px(2.)).bg(rgb(0xDDE8EE)))
                .child(div().w(px(48.)).h(px(4.)).rounded(px(2.)).bg(rgb(0x66B5E5)))
                .child(div().w(px(78.)).h(px(4.)).rounded(px(2.)).bg(rgb(0xA8C2D1)))
                .child(div().w(px(30.)).h(px(4.)).rounded(px(2.)).bg(rgb(0x58C99A))),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .gap(px(6.))
                .child(
                    div()
                        .text_size(px(9.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(rgb(0x77818E))
                        .child("BUILT-IN · DARK"),
                )
                .child(
                    div()
                        .text_size(px(14.))
                        .font_weight(FontWeight(650.))
                        .text_color(rgb(0xF0F3F5))
                        .child("Cipher Midnight"),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(5.))
                        .child(div().size(px(9.)).rounded(px(3.)).bg(rgb(ACCENT)))
                        .child(div().size(px(9.)).rounded(px(3.)).bg(rgb(0x65B58D)))
                        .child(div().size(px(9.)).rounded(px(3.)).bg(rgb(0xD2B45B)))
                        .child(div().size(px(9.)).rounded(px(3.)).bg(rgb(0x9B7BD7)))
                        .child(div().size(px(9.)).rounded(px(3.)).bg(rgb(0xD96B76))),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(7.))
                .text_size(px(11.))
                .font_weight(FontWeight(550.))
                .text_color(rgb(0x9ECBE7))
                .child("Change theme")
                .child(
                    Icon::new(IconName::ChevronRight)
                        .size(px(14.))
                        .text_color(rgb(0x78B8DE)),
                ),
        )
        .into_any_element()
}

fn opacity_control(locker: Entity<Nox>, value: u8) -> AnyElement {
    let rail_width = 196.;
    let knob = ((value.saturating_sub(60) as f32 / 40.) * rail_width).round();
    div()
        .id("settings-opacity")
        .flex()
        .items_center()
        .gap(px(10.))
        .cursor_pointer()
        .hover(|this| this.opacity(0.82))
        .child(
            div()
                .w(px(210.))
                .h(px(16.))
                .relative()
                .flex()
                .items_center()
                .child(
                    div()
                        .absolute()
                        .w_full()
                        .h(px(4.))
                        .rounded(px(2.))
                        .bg(rgb(0x3A434E)),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(knob))
                        .top(px(1.))
                        .w(px(14.))
                        .h(px(14.))
                        .rounded_full()
                        .bg(rgb(0xDDECF5))
                        .border_1()
                        .border_color(rgb(0x6FAED4)),
                ),
        )
        .child(
            div()
                .text_size(px(10.))
                .font_weight(FontWeight(650.))
                .text_color(rgb(0xDDE5EB))
                .child(format!("{value}%")),
        )
        .on_click(move |_, window, app| {
            update_settings(locker.clone(), window, app, |settings| {
                settings.transparency_percent = if settings.transparency_percent >= 100 {
                    60
                } else {
                    settings.transparency_percent + 4
                };
            });
        })
        .into_any_element()
}

fn language_control(theme: Theme, locker: Entity<Nox>, language: &str) -> AnyElement {
    let label = language.to_owned();
    div()
        .id("settings-language")
        .w(px(190.))
        .h(px(36.))
        .px(px(11.))
        .flex()
        .items_center()
        .justify_between()
        .rounded(px(7.))
        .cursor_pointer()
        .hover(|this| this.bg(theme.row_hover))
        .bg(rgb(INPUT))
        .border_1()
        .border_color(rgb(INPUT_BORDER))
        .child(
            div()
                .text_size(px(11.))
                .font_weight(FontWeight(550.))
                .text_color(rgb(0xDDE3E8))
                .child(label),
        )
        .child(
            Icon::new(IconName::ChevronDown)
                .size(px(14.))
                .text_color(rgb(0x7E8995)),
        )
        .on_click(move |_, window, app| {
            update_settings(locker.clone(), window, app, |settings| {
                settings.language = if settings.language == "English" {
                    "Español".into()
                } else {
                    "English".into()
                };
            });
        })
        .into_any_element()
}

fn security_section(theme: Theme, locker: Entity<Nox>, settings: Settings) -> AnyElement {
    let auto_lock = duration_control(
        theme,
        "settings-auto-lock",
        locker.clone(),
        settings.auto_lock_seconds,
        |settings| &mut settings.auto_lock_seconds,
    );
    let clipboard = duration_control(
        theme,
        "settings-clipboard-clear",
        locker,
        settings.clipboard_seconds,
        |settings| &mut settings.clipboard_seconds,
    );
    div()
        .flex()
        .flex_col()
        .gap(px(22.))
        .child(section_intro(
            "Security",
            "Keep the vault protected when you step away.",
        ))
        .child(settings_row(
            "Lock after inactivity",
            "Nox locks automatically after this period without activity.",
            auto_lock,
        ))
        .child(divider())
        .child(settings_row(
            "Clear copied secrets",
            "Secrets copied from Nox are removed from the clipboard automatically.",
            clipboard,
        ))
        .into_any_element()
}

fn duration_control(
    theme: Theme,
    id: &'static str,
    locker: Entity<Nox>,
    seconds: u64,
    field: fn(&mut Settings) -> &mut u64,
) -> AnyElement {
    div()
        .id(id)
        .w(px(120.))
        .h(px(36.))
        .px(px(11.))
        .flex()
        .items_center()
        .justify_between()
        .rounded(px(7.))
        .cursor_pointer()
        .hover(|this| this.bg(theme.row_hover))
        .bg(rgb(INPUT))
        .border_1()
        .border_color(rgb(INPUT_BORDER))
        .child(
            div()
                .text_size(px(11.))
                .text_color(rgb(PRIMARY))
                .child(format_duration(seconds)),
        )
        .child(div().text_size(px(12.)).text_color(rgb(MUTED)).child("⌄"))
        .on_click(move |_, window, app| {
            update_settings(locker.clone(), window, app, move |settings| {
                let value = field(settings);
                *value = match *value {
                    30 => 60,
                    60 => 300,
                    300 => 900,
                    900 => 1800,
                    _ => 30,
                };
            });
        })
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
        SEARCH_PLACEHOLDER
    } else {
        0xE1E5E9
    };
    let meta_color = if missing {
        SEARCH_PLACEHOLDER
    } else {
        NAV_LABEL
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
        .bg(rgb(INPUT))
        // The 1px box is always there, transparent at rest: revealing a border
        // on hover would otherwise shift every row by a pixel.
        .border_1()
        .border_color(rgba(0x00000000))
        .hover(|row| row.bg(rgb(ACTIVE)).border_color(rgb(ACTIVE_BORDER)))
        .child(
            div()
                .size(px(32.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(rgb(ACTIVE))
                // Without this the avatar dissolves into the hovered row,
                // which lifts to the same colour.
                .group_hover(group.clone(), |avatar| avatar.bg(rgb(ACTIVE_BORDER)))
                .child(
                    div()
                        .text_size(px(10.))
                        .font_weight(FontWeight(700.))
                        .text_color(rgb(if missing {
                            SEARCH_PLACEHOLDER
                        } else {
                            0xDDE3E8
                        }))
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
                                .text_color(rgb(name_color))
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
                                    .bg(rgb(ACTIVE))
                                    .border_1()
                                    .border_color(rgb(ACTIVE_BORDER))
                                    .child(
                                        div()
                                            .text_size(px(8.))
                                            .font_weight(FontWeight(700.))
                                            .text_color(rgb(ACTIVE_TEXT))
                                            .child("ACTIVE"),
                                    ),
                            )
                        }),
                )
                .child(
                    div()
                        .text_size(px(10.))
                        .text_color(rgb(meta_color))
                        .child(meta),
                ),
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
                            .text_color(rgb(MUTED)),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight(550.))
                            .text_color(rgb(SECONDARY))
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
                        .text_color(rgb(MUTED)),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .font_weight(FontWeight(550.))
                        .text_color(rgb(SECONDARY))
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
            "Vault",
            "Manage the encrypted vaults stored on this device.",
        ))
        .child(div().flex().flex_col().gap(px(8.)).w_full().children(
            vault_list.entries.iter().enumerate().map(|(index, vault)| {
                vault_row(
                    rows_locker.clone(),
                    index,
                    vault,
                    vault_list.active_id.as_deref() == Some(vault.id.as_str()),
                    conflict_count,
                )
            }),
        ))
        .child(divider())
        .child(settings_row(
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
        .child(divider())
        .child(settings_row(
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

fn import_export_section(locker: Entity<Nox>) -> AnyElement {
    let export = locker.clone();
    let restore = locker;
    div()
        .flex()
        .flex_col()
        .gap(px(22.))
        .child(section_intro(
            "Import & export",
            "Move encrypted backups without exposing your vault.",
        ))
        .child(settings_row(
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
        .child(divider())
        .child(settings_row(
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

fn privacy_section(theme: Theme, locker: Entity<Nox>, settings: Settings) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(22.))
        .child(section_intro("Privacy", "Reduce the amount of secret data left visible on screen."))
        .child(settings_row(
            "Clear copied secrets",
            "Remove copied credentials from the clipboard after the selected delay.",
            duration_control(theme, "settings-privacy-clipboard", locker, settings.clipboard_seconds, |settings| &mut settings.clipboard_seconds),
        ))
        .child(divider())
        .child(
            div()
                .rounded(px(8.))
                .p(px(14.))
                .bg(rgb(SURFACE))
                .border_1()
                .border_color(rgb(INPUT_BORDER))
                .text_size(px(11.))
                .text_color(rgb(MUTED))
                .child("Nox does not send vault contents, search terms, or copied secrets to any account service."),
        )
        .into_any_element()
}

/// A capability this build does not have, described in one place.
///
/// The four strings and the two `fn` pointers all describe the same
/// capability, and every field is `&'static` or a function pointer — so each
/// one is a `const` rather than six positional arguments. It also makes a bug
/// class unrepresentable: `read` and `toggle` used to be loose pointers passed
/// four arguments away from the copy they belong to, so nothing stopped a
/// caller pairing Autofill's strings with Notifications' toggle.
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

const NOTIFICATIONS: UnavailableCapability = UnavailableCapability {
    title: "Notifications",
    subtitle: "Stay informed without exposing vault data",
    unavailable: "System notifications are unavailable until Nox has an operating-system \
                  notification service.",
    label: "Enable desktop notifications when available",
    read: |settings| settings.notifications_enabled,
    toggle: |settings| settings.notifications_enabled = !settings.notifications_enabled,
};

fn availability_section(
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
        .child(section_intro(title, subtitle))
        .child(settings_row(label, unavailable, toggle("settings-availability", enabled, locker, change)))
        .child(
            div()
                .rounded(px(8.))
                .p(px(14.))
                .bg(rgb(SURFACE))
                .border_1()
                .border_color(rgb(INPUT_BORDER))
                .text_size(px(11.))
                .text_color(rgb(MUTED))
                .child("Your preference is saved locally and will be used when this capability is available."),
        )
        .into_any_element()
}

fn account_section() -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(22.))
        .child(section_intro("Account", "Nox is local-first and this vault is not connected to an account."))
        .child(
            div()
                .rounded(px(8.))
                .p(px(16.))
                .flex()
                .flex_col()
                .gap(px(6.))
                .bg(rgb(SURFACE))
                .border_1()
                .border_color(rgb(INPUT_BORDER))
                .child(div().text_size(px(12.)).text_color(rgb(PRIMARY)).child("No account connected"))
                .child(div().text_size(px(11.)).text_color(rgb(MUTED)).child("Your vault remains encrypted on this device. Account sync is not part of this build.")),
        )
        .into_any_element()
}
