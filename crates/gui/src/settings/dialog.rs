use super::super::Locker;
use super::Settings;
use gpui::{
    Animation, AnimationExt, AnyElement, App, BoxShadow, Entity, FontWeight, MouseButton, Window,
    div, ease_out_quint, point, prelude::*, px, rgb, rgba,
};
use gpui_component::{Icon, IconName, Sizable, button::Button};
use std::time::Duration;

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
    locker: Entity<Locker>,
    settings: Settings,
    section: SettingsSection,
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
            let _ = close_locker.update(app, |locker, cx| {
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
                    locker,
                    settings,
                    section,
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
    locker: Entity<Locker>,
    settings: Settings,
    section: SettingsSection,
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
                    locker.clone(),
                    settings,
                    section,
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
                    let _ = close_locker.update(cx, |locker, cx| {
                        locker.close_settings_dialog(window, cx);
                    });
                })
        })
        .into_any_element()
}

fn render_navigation(locker: Entity<Locker>, selected: SettingsSection) -> AnyElement {
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
    locker: Entity<Locker>,
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
            let _ = locker.update(app, |locker, cx| {
                locker.settings_section = section;
                cx.notify();
            });
        })
        .into_any_element()
}

fn render_section(
    locker: Entity<Locker>,
    settings: Settings,
    section: SettingsSection,
    conflict_count: usize,
) -> AnyElement {
    match section {
        SettingsSection::Appearance => appearance_section(locker, settings),
        SettingsSection::Security => security_section(locker, settings),
        SettingsSection::Vault => vault_section(locker, conflict_count),
        SettingsSection::Autofill => availability_section(
            locker,
            settings,
            "Autofill",
            "Fill credentials without revealing them",
            "Autofill needs the Nox browser extension, which is not installed in this build.",
            "Enable autofill when the extension is available",
            |settings| settings.autofill_enabled = !settings.autofill_enabled,
            |settings| settings.autofill_enabled,
        ),
        SettingsSection::Privacy => privacy_section(locker, settings),
        SettingsSection::Notifications => availability_section(
            locker,
            settings,
            "Notifications",
            "Stay informed without exposing vault data",
            "System notifications are unavailable until Nox has an operating-system notification service.",
            "Enable desktop notifications when available",
            |settings| settings.notifications_enabled = !settings.notifications_enabled,
            |settings| settings.notifications_enabled,
        ),
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
    locker: Entity<Locker>,
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

fn update_settings<F>(locker: Entity<Locker>, window: &mut Window, app: &mut App, change: F)
where
    F: FnOnce(&mut Settings),
{
    let _ = locker.update(app, |locker, cx| {
        let mut settings = locker.settings.clone();
        change(&mut settings);
        locker.update_settings(settings, window, cx);
    });
}

fn appearance_section(locker: Entity<Locker>, settings: Settings) -> AnyElement {
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
    let language = language_control(locker, &settings.language);

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

fn opacity_control(locker: Entity<Locker>, value: u8) -> AnyElement {
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

fn language_control(locker: Entity<Locker>, language: &str) -> AnyElement {
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
        .hover(|this| this.bg(rgb(0x29313C)))
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

fn security_section(locker: Entity<Locker>, settings: Settings) -> AnyElement {
    let auto_lock = duration_control(
        "settings-auto-lock",
        locker.clone(),
        settings.auto_lock_seconds,
        |settings| &mut settings.auto_lock_seconds,
    );
    let clipboard = duration_control(
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
    id: &'static str,
    locker: Entity<Locker>,
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
        .hover(|this| this.bg(rgb(0x29313C)))
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

fn vault_section(locker: Entity<Locker>, conflict_count: usize) -> AnyElement {
    let restore = locker.clone();
    let export = locker.clone();
    let conflicts = locker;
    div()
        .flex()
        .flex_col()
        .gap(px(22.))
        .child(section_intro(
            "Vault",
            "Manage encrypted data stored on this device.",
        ))
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
                        let _ = restore.update(app, |locker, cx| locker.begin_restore(window, cx));
                    },
                ))
                .child(action_button(
                    "settings-export-backup",
                    "Export",
                    move |window, app| {
                        let _ = export.update(app, |locker, cx| locker.begin_export(window, cx));
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
                    let _ = conflicts.update(app, |locker, cx| {
                        locker.open_conflicts(window, cx);
                        locker.close_settings_dialog(window, cx);
                    });
                },
            ),
        ))
        .into_any_element()
}

fn import_export_section(locker: Entity<Locker>) -> AnyElement {
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
                    let _ = export.update(app, |locker, cx| locker.begin_export(window, cx));
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
                    let _ = restore.update(app, |locker, cx| locker.begin_restore(window, cx));
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

fn privacy_section(locker: Entity<Locker>, settings: Settings) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(px(22.))
        .child(section_intro("Privacy", "Reduce the amount of secret data left visible on screen."))
        .child(settings_row(
            "Clear copied secrets",
            "Remove copied credentials from the clipboard after the selected delay.",
            duration_control("settings-privacy-clipboard", locker, settings.clipboard_seconds, |settings| &mut settings.clipboard_seconds),
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

fn availability_section(
    locker: Entity<Locker>,
    settings: Settings,
    title: &'static str,
    subtitle: &'static str,
    unavailable: &'static str,
    label: &'static str,
    change: fn(&mut Settings),
    value: fn(&Settings) -> bool,
) -> AnyElement {
    let enabled = value(&settings);
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
