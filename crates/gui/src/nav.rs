//! App-level sidebar: Vault navigation and the Settings entry point. The
//! Pencil "Nox — Home" frame carries no brand mark inside the sidebar itself
//! (only the title bar's small logo), so this doesn't render one either.

use super::Locker;
use gpui::{AnyElement, Context, FontWeight, Window, div, prelude::*, px, rgb};
use gpui_component::{
    Icon, IconName, Sizable, WindowExt,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
};
use gpui_rsx::rsx;
use locker_core::ItemType;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ActiveView {
    Home,
    AllItems,
    Logins,
    SecureNotes,
}

impl ActiveView {
    pub(crate) fn item_type(self) -> Option<ItemType> {
        match self {
            ActiveView::Home => None,
            ActiveView::AllItems => None,
            ActiveView::Logins => Some(ItemType::Login),
            ActiveView::SecureNotes => Some(ItemType::SecureNote),
        }
    }
}

impl Locker {
    /// Switch the active nav view, resetting selection-scoped UI state that
    /// no longer applies (a Login's password may have been revealed, etc).
    pub(crate) fn set_active_view(&mut self, view: ActiveView, cx: &mut Context<Self>) {
        if self.active_view == view {
            return;
        }
        self.active_view = view;
        self.reveal_password = false;
        if let Some(list) = self.vault_list.as_mut() {
            list.set_type_filter(view.item_type());
        }
        cx.notify();
    }

    pub(crate) fn open_settings_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let locker = cx.entity();
        let conflict_count = self.conflicts.count();
        window.open_dialog(cx, move |dialog, _window, _cx| {
            dialog
                .w(px(1216.))
                .h(px(872.))
                .p(px(0.))
                .gap(px(0.))
                .rounded(px(10.))
                .close_button(false)
                .overlay_closable(true)
                .child(render_settings_modal(locker.clone(), conflict_count))
        });
    }

    pub(crate) fn render_sidebar_nav(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let active = self.active_view;
        let locker = cx.entity();
        let locker_home = locker.clone();
        let locker_all_items = locker.clone();
        let locker_logins = locker.clone();
        let locker_notes = locker.clone();
        let locker_settings = locker.clone();

        rsx! {
            <div
                id="app-sidebar"
                flex flex_col
                w={px(224.)} h_full flex_shrink_0
                bg={rgb(super::CIPHER_BACKGROUND)}
                border_r_1 borderColor={rgb(super::CIPHER_BORDER)}
                px={px(14.)} pt={px(16.)} pb={px(16.)}
            >
                <div flex flex_col gap={px(SIDEBAR_ROW_GAP)}>
                    {sidebar_section_label("VAULT")}
                    {sidebar_link("sidebar-home", "icons/house.svg", "Home", active == ActiveView::Home, move |_, _window, app| {
                        locker_home.update(app, |locker, cx| locker.set_active_view(ActiveView::Home, cx));
                    }, cx)}
                    {sidebar_link("sidebar-all-items", "icons/layout-grid.svg", "All items", active == ActiveView::AllItems, move |_, _window, app| {
                        locker_all_items.update(app, |locker, cx| locker.set_active_view(ActiveView::AllItems, cx));
                    }, cx)}
                    // ponytail: visual-only until the vault model owns favorite state; add filtering when that state exists.
                    {sidebar_static_item("icons/star.svg", "Favorites")}
                    {sidebar_link("sidebar-logins", "icons/key-round.svg", "Logins", active == ActiveView::Logins, move |_, _window, app| {
                        locker_logins.update(app, |locker, cx| locker.set_active_view(ActiveView::Logins, cx));
                    }, cx)}
                    {sidebar_static_item("icons/credit-card.svg", "Cards")}
                    {sidebar_link("sidebar-secure-notes", "icons/file-lock.svg", "Secure notes", active == ActiveView::SecureNotes, move |_, _window, app| {
                        locker_notes.update(app, |locker, cx| locker.set_active_view(ActiveView::SecureNotes, cx));
                    }, cx)}
                    {sidebar_static_item("icons/contact.svg", "Identities")}
                </div>
                // The Pencil frame positions the Tools group 92px below the end
                // of the Vault group (y 442 vs 350).
                <div h={px(92.)} flex_shrink_0 />
                <div flex flex_col gap={px(SIDEBAR_ROW_GAP)}>
                    {sidebar_section_label("TOOLS")}
                    {sidebar_static_item("icons/shield-check.svg", "Security report")}
                    {sidebar_static_item("icons/wand-sparkles.svg", "Password generator")}
                </div>
                <div flex_1 />
                {sidebar_link("open-settings", "icons/settings.svg", "Settings", false, move |_, window, app| {
                    locker_settings.update(app, |locker, cx| locker.open_settings_dialog(window, cx));
                }, cx)}
            </div>
        }
        .into_any_element()
    }
}

const SIDEBAR_ROW_GAP: f32 = 6.;
const SIDEBAR_ROW_HEIGHT: f32 = 40.;
const SIDEBAR_ROW_PADDING_X: f32 = 11.;
const SIDEBAR_ROW_RADIUS: f32 = 8.;
const SIDEBAR_ICON_SIZE: f32 = 17.;
const SIDEBAR_LABEL_SIZE: f32 = 13.;
const SIDEBAR_SECTION_LABEL_SIZE: f32 = 9.;

fn sidebar_section_label(label: &'static str) -> AnyElement {
    // ponytail: the design also specifies 0.8px letter-spacing, which GPUI has
    // no API for; drop it rather than fake it with per-character elements.
    div()
        .text_size(px(SIDEBAR_SECTION_LABEL_SIZE))
        .font_weight(FontWeight(700.))
        .text_color(rgb(super::CIPHER_FOREGROUND_MUTED))
        .child(label)
        .into_any_element()
}

/// `full_width`: whether this fills its row (a `sidebar_link` button, where it
/// must neutralize `Button`'s own forced `justify_center` — see below) or
/// stays sized to its content, leaving room for a trailing badge
/// (`sidebar_static_item`).
fn sidebar_row_content(
    icon_path: &'static str,
    label: &'static str,
    active: bool,
    full_width: bool,
) -> AnyElement {
    let color = rgb(if active {
        super::CIPHER_FOREGROUND_SECONDARY
    } else {
        super::CIPHER_FOREGROUND_MUTED
    });
    div()
        .flex()
        .items_center()
        .min_w(px(0.))
        .gap(px(10.))
        .when_else(
            full_width,
            |this| this.w_full(),
            // Not full-width: this sits next to a trailing "Soon" badge in
            // `sidebar_static_item`, so it must be able to shrink (and its
            // label truncate) rather than push the badge out of the row.
            |this| this.flex_1(),
        )
        .child(
            Icon::empty()
                .path(icon_path)
                .size(px(SIDEBAR_ICON_SIZE))
                .text_color(color)
                .flex_shrink_0(),
        )
        .child(
            div()
                .min_w(px(0.))
                .overflow_hidden()
                .truncate()
                .text_size(px(SIDEBAR_LABEL_SIZE))
                .font_weight(FontWeight(if active { 650. } else { 500. }))
                .text_color(color)
                .child(label),
        )
        .into_any_element()
}

/// A clickable Vault/Tools row. `active` both selects the highlighted style
/// and (for the always-`false` Settings footer row) just means "never
/// highlighted" — it still opens its dialog on click.
///
/// `gpui_component::button::Button`'s internal content wrapper is hardcoded
/// `.justify_center()` with no way to override it directly, so a label
/// narrower than the row centers instead of sitting at the left — the actual
/// "why is this centered" bug. Rather than hand-roll Button's keyboard
/// accessibility (tab stop, Enter/Space activation) on a plain div, this
/// keeps Button and neutralizes the centering by giving it a single
/// `w_full()` child: centering content that already fills the width is a
/// no-op, and that child's own (default flex-start) layout is what actually
/// places the icon and label at the left.
fn sidebar_link(
    id: &'static str,
    icon_path: &'static str,
    label: &'static str,
    active: bool,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut gpui::App) + 'static,
    cx: &mut Context<Locker>,
) -> AnyElement {
    let resting_bg = if active {
        super::CIPHER_SURFACE_RAISED
    } else {
        super::CIPHER_BACKGROUND
    };
    let foreground = if active {
        super::CIPHER_FOREGROUND_SECONDARY
    } else {
        super::CIPHER_FOREGROUND_MUTED
    };
    // The user asked for hover to look exactly like the active state, so the
    // hover/active colors are the active background regardless of `active`.
    let variant = ButtonCustomVariant::new(cx)
        .color(rgb(resting_bg).into())
        .hover(rgb(super::CIPHER_SURFACE_RAISED).into())
        .active(rgb(super::CIPHER_SURFACE_RAISED).into())
        .foreground(rgb(foreground).into());
    Button::new(id)
        .custom(variant)
        .w_full()
        .h(px(SIDEBAR_ROW_HEIGHT))
        .px(px(SIDEBAR_ROW_PADDING_X))
        .rounded(px(SIDEBAR_ROW_RADIUS))
        .on_click(on_click)
        .child(sidebar_row_content(icon_path, label, active, true))
        .into_any_element()
}

/// A row for a nav destination that doesn't exist yet (Favorites, Cards,
/// Identities, Security report, Password generator): same look as an inactive
/// link plus a "Soon" badge, and not clickable.
fn sidebar_static_item(icon_path: &'static str, label: &'static str) -> AnyElement {
    div()
        .w_full()
        .h(px(SIDEBAR_ROW_HEIGHT))
        .px(px(SIDEBAR_ROW_PADDING_X))
        .rounded(px(SIDEBAR_ROW_RADIUS))
        .flex()
        .items_center()
        .justify_between()
        .child(sidebar_row_content(icon_path, label, false, false))
        .child(
            div()
                .flex_shrink_0()
                .ml(px(8.))
                .text_size(px(10.))
                .font_weight(FontWeight(600.))
                .text_color(rgb(super::CIPHER_FOREGROUND_MUTED))
                .bg(rgb(super::CIPHER_SURFACE_RAISED))
                .rounded(px(4.))
                .px(px(6.))
                .py(px(2.))
                .child("Soon"),
        )
        .into_any_element()
}

fn settings_nav_item(icon_path: &'static str, label: &'static str, active: bool) -> AnyElement {
    div()
        .w_full()
        .h(px(38.))
        .px(px(10.))
        .flex()
        .items_center()
        .gap(px(10.))
        .rounded(px(7.))
        .when(active, |this| {
            this.bg(rgb(0xE8F3F9))
                .border_1()
                .border_color(rgb(0xB9DDF0))
                .text_color(rgb(0x2878A8))
        })
        .when(!active, |this| this.text_color(rgb(0x4F5660)))
        .child(Icon::empty().path(icon_path).text_color(if active {
            rgb(0x2878A8)
        } else {
            rgb(0x69717B)
        }))
        .child(div().text_sm().child(label))
        .into_any_element()
}

fn settings_toggle(enabled: bool) -> AnyElement {
    div()
        .w(px(36.))
        .h(px(20.))
        .px(px(3.))
        .flex()
        .items_center()
        .rounded_full()
        .when(enabled, |this| this.justify_end().bg(rgb(0x4A9FD8)))
        .when(!enabled, |this| this.justify_start().bg(rgb(0xDDE0E5)))
        .child(div().size(px(14.)).rounded_full().bg(if enabled {
            rgb(0xFFFFFF)
        } else {
            rgb(0x9FA8B1)
        }))
        .into_any_element()
}

fn settings_row(label: &'static str, description: &'static str, control: AnyElement) -> AnyElement {
    rsx! {
        <div flex items_center justify_between h={px(42.)}>
            <div flex flex_col gap={px(4.)}>
                <div text_sm textColor={rgb(0x20242A)}>{label}</div>
                <div text_xs textColor={rgb(0x69717B)}>{description}</div>
            </div>
            {control}
        </div>
    }
    .into_any_element()
}

fn render_settings_modal(locker: gpui::Entity<Locker>, conflict_count: usize) -> AnyElement {
    let lock_locker = locker.clone();
    let restore_locker = locker.clone();
    let export_locker = locker.clone();
    let conflicts_locker = locker.clone();
    // ponytail: appearance controls remain display-only until preferences have durable storage.
    rsx! {
        <div id="settings-modal" relative flex size_full overflow_hidden bg={rgb(0xF7F8FA)}>
            <div id="settings-navigation" w={px(236.)} h_full flex flex_col gap={px(12.)} p={px(16.)} pt={px(24.)} flex_shrink_0 bg={rgb(0xF7F8FA)} border_r_1 borderColor={rgb(0xDDE0E5)}>
                <div text_xs fontWeight={FontWeight::BOLD} textColor={rgb(0x69717B)}>{"SETTINGS"}</div>
                <div h={px(38.)} px={px(11.)} flex items_center gap={px(9.)} rounded={px(7.)} bg={rgb(0xFFFFFF)} border_1 borderColor={rgb(0xDDE0E5)}>
                    <Icon base={Icon::empty().path("icons/search.svg").text_color(rgb(0x69717B))} />
                    <div text_xs textColor={rgb(0x777C85)}>{"Search settings…"}</div>
                </div>
                <div flex flex_col gap={px(5.)}>
                    {settings_nav_item("icons/pencil-sparkles.svg", "Appearance", true)}
                    {settings_nav_item("icons/shield-check.svg", "Security", false)}
                    {settings_nav_item("icons/database.svg", "Vault", false)}
                    {settings_nav_item("icons/key-square.svg", "Autofill", false)}
                    {settings_nav_item("icons/eye-off.svg", "Privacy", false)}
                    {settings_nav_item("icons/bell.svg", "Notifications", false)}
                    {settings_nav_item("icons/arrow-left-right.svg", "Import & export", false)}
                    {settings_nav_item("icons/circle-user-around.svg", "Account", false)}
                </div>
                <div flex_1 />
                <Button
                    base={Button::new("settings-lock-vault")
                        .ghost()
                        .w_full()
                        .justify_start()
                        .icon(Icon::empty().path("icons/lock-keyhole.svg"))
                        .label("Lock vault now")
                        .on_click(move |_, window, app| {
                            lock_locker.update(app, |locker, cx| locker.lock_vault(window, cx));
                        })}
                />
            </div>
            <div id="settings-appearance" flex flex_col flex_1 min_w={px(0.)} h_full p={px(34.)} pt={px(26.)} gap={px(22.)} bg={rgb(0xF7F8FA)} overflow_y_scroll>
                <Button
                    base={Button::new("close-settings")
                        .ghost()
                        .absolute()
                        .top(px(14.))
                        .right(px(14.))
                        .icon(IconName::Close)
                        .on_click(|_, window, cx| window.close_dialog(cx))}
                />
                <div flex flex_col gap={px(14.)}>
                    <div text_xl textColor={rgb(0x20242A)}>{"Theme"}</div>
                    <div text_xs textColor={rgb(0x69717B)}>{"Choose how Locker looks across your devices."}</div>
                    {settings_row("Sync with system", "Follow your operating system appearance automatically.", settings_toggle(false))}
                    {settings_row("Legible bright colors", "Improve contrast for bright accent colors.", settings_toggle(true))}
                    <div h={px(94.)} p={px(12.)} flex items_center gap={px(16.)} rounded={px(8.)} bg={rgb(0xF5F6F8)} border_1 borderColor={rgb(0xDDE0E5)}>
                        <div w={px(150.)} h={px(68.)} p={px(11.)} flex flex_col gap={px(7.)} rounded={px(6.)} bg={rgb(0xFFFFFF)}>
                            <div w={px(66.)} h={px(4.)} rounded={px(2.)} bg={rgb(0x20242A)} />
                            <div w={px(48.)} h={px(4.)} rounded={px(2.)} bg={rgb(0x4A9FD8)} />
                            <div w={px(78.)} h={px(4.)} rounded={px(2.)} bg={rgb(0x8AAAC0)} />
                            <div w={px(30.)} h={px(4.)} rounded={px(2.)} bg={rgb(0x58C99A)} />
                        </div>
                        <div flex flex_col flex_1 gap={px(6.)}>
                            <div text_xs fontWeight={FontWeight::BOLD} textColor={rgb(0x69717B)}>{"BUILT-IN · LIGHT"}</div>
                            <div text_sm textColor={rgb(0x20242A)}>{"Locker Daylight"}</div>
                            <div flex gap={px(5.)}>
                                <div size={px(9.)} rounded={px(3.)} bg={rgb(0x4A9FD8)} />
                                <div size={px(9.)} rounded={px(3.)} bg={rgb(0x58C99A)} />
                                <div size={px(9.)} rounded={px(3.)} bg={rgb(0xD2B45B)} />
                                <div size={px(9.)} rounded={px(3.)} bg={rgb(0x9B7BD7)} />
                                <div size={px(9.)} rounded={px(3.)} bg={rgb(0xD96B76)} />
                            </div>
                        </div>
                        <div flex items_center gap={px(7.)} text_xs textColor={rgb(0x2878A8)}>
                            <div>{"Change theme"}</div>
                            <div>{"›"}</div>
                        </div>
                    </div>
                </div>
                <div h={px(1.)} w_full bg={rgb(0xE4E6EA)} />
                <div flex flex_col gap={px(13.)}>
                    <div text_lg textColor={rgb(0x20242A)}>{"Transparency"}</div>
                    {settings_row("Opacity", "Adjust the background transparency of the vault.", rsx! {
                        <div flex items_center gap={px(10.)}>
                            <div w={px(210.)} h={px(4.)} rounded={px(2.)} bg={rgb(0xDDE0E5)}>
                                <div w={px(14.)} h={px(14.)} rounded_full bg={rgb(0xFFFFFF)} border_1 borderColor={rgb(0x4A9FD8)} />
                            </div>
                            <div text_xs textColor={rgb(0x4F5660)}>{"100%"}</div>
                        </div>
                    }.into_any_element())}
                    {settings_row("Background blur", "Blur content behind the vault window.", settings_toggle(true))}
                    {settings_row("Dim inactive panes", "Reduce contrast in unfocused panels.", settings_toggle(true))}
                </div>
                <div h={px(1.)} w_full bg={rgb(0xE4E6EA)} />
                <div flex flex_col gap={px(12.)}>
                    <div text_lg textColor={rgb(0x20242A)}>{"Language"}</div>
                    {settings_row("Interface language", "Choose the language used throughout Locker.", rsx! {
                        <div w={px(190.)} h={px(36.)} px={px(11.)} flex items_center justify_between rounded={px(7.)} bg={rgb(0xFFFFFF)} border_1 borderColor={rgb(0xDDE0E5)}>
                            <div text_xs textColor={rgb(0x20242A)}>{"English"}</div>
                            <div text_xs textColor={rgb(0x69717B)}>{"⌄"}</div>
                        </div>
                    }.into_any_element())}
                </div>
                <div h={px(1.)} w_full bg={rgb(0xE4E6EA)} />
                <div flex flex_col gap={px(12.)}>
                    <div text_lg textColor={rgb(0x20242A)}>{"Vault administration"}</div>
                    {settings_row("Backups", "Export a recovery copy or restore one safely.", rsx! {
                        <div flex items_center gap={px(8.)}>
                            <Button
                                base={Button::new("settings-restore-backup")
                                    .outline()
                                    .small()
                                    .label("Restore")
                                    .on_click(move |_, window, app| {
                                        restore_locker.update(app, |locker, cx| locker.begin_restore(window, cx));
                                    })}
                            />
                            <Button
                                base={Button::new("settings-export-backup")
                                    .outline()
                                    .small()
                                    .label("Export")
                                    .on_click(move |_, window, app| {
                                        export_locker.update(app, |locker, cx| locker.begin_export(window, cx));
                                    })}
                            />
                        </div>
                    }.into_any_element())}
                    {settings_row("Conflicts", "Review changes that need your decision.", rsx! {
                        <Button
                            base={Button::new("settings-conflicts")
                                .outline()
                                .small()
                                .label(format!("Review ({conflict_count})"))
                                .on_click(move |_, window, app| {
                                    conflicts_locker.update(app, |locker, cx| locker.open_conflicts(window, cx));
                                    window.close_dialog(app);
                                })}
                        />
                    }.into_any_element())}
                </div>
            </div>
        </div>
    }
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::ActiveView;
    use crate::assets::Assets;
    use gpui::AssetSource;
    use locker_core::ItemType;

    /// Every `icons/…svg` path referenced in the GUI's rendering code must
    /// actually resolve, from our embedded set or gpui-component's bundled
    /// fallback. An unresolved path renders as a blank gap instead of failing
    /// loudly, so nothing else catches a typo or a not-yet-added icon.
    #[test]
    fn every_icon_path_in_gui_source_resolves() {
        let mut checked = 0;
        for source in [
            include_str!("nav.rs"),
            include_str!("app.rs"),
            include_str!("vault_list.rs"),
            include_str!("detail.rs"),
            include_str!("item_editor.rs"),
        ] {
            for (index, _) in source.match_indices("\"icons/") {
                let path = source[index + 1..].split('"').next().expect("closing quote");
                // Skips this test's own `"icons/` search literal, which has no suffix.
                if !path.ends_with(".svg") {
                    continue;
                }
                assert!(
                    matches!(Assets.load(path), Ok(Some(_))),
                    "icon {path} does not resolve"
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "expected some icon paths to check");
    }

    #[test]
    fn secure_note_toolbar_icons_resolve() {
        for path in [
            "icons/arrow-left.svg",
            "icons/bold.svg",
            "icons/italic.svg",
            "icons/list.svg",
            "icons/code.svg",
        ] {
            assert!(
                matches!(Assets.load(path), Ok(Some(_))),
                "icon {path} does not resolve"
            );
        }
    }

    #[test]
    fn secure_note_workspace_uses_requested_icons_and_pins_encryption_footer() {
        let source = include_str!("item_editor.rs");
        for icon in ["arrow-left", "bold", "italic", "list", "code"] {
            assert!(
                source.contains(&format!("\"icons/{icon}.svg\"")),
                "secure-note workspace should use {icon}"
            );
        }
        assert!(source.contains(
            "{field(\"CONTENT\", true, note_editor)}\n                            {error}\n                            <div flex_1 />\n                            <div flex items_center h={px(42.)}",
        ));
    }

    #[test]
    fn all_items_view_has_no_item_type_filter() {
        assert_eq!(ActiveView::AllItems.item_type(), None);
        assert_eq!(ActiveView::Logins.item_type(), Some(ItemType::Login));
        assert_eq!(
            ActiveView::SecureNotes.item_type(),
            Some(ItemType::SecureNote)
        );
    }
}
