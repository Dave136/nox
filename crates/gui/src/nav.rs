//! App-level sidebar: Vault navigation and the Settings entry point. The
//! Pencil "Nox — Home" frame carries no brand mark inside the sidebar itself
//! (only the title bar's small logo), so this doesn't render one either.

use crate::app::Nox;
use crate::settings;
use gpui::{AnyElement, Context, FontWeight, Window, div, prelude::*, px, rgb};
use gpui_component::{
    Icon,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
};
use gpui_rsx::rsx;
use nox_core::ItemType;

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

impl Nox {
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

    pub(crate) fn open_settings_dialog(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.settings_section = settings::SettingsSection::Appearance;
        self.settings_open = true;
        cx.notify();
    }

    pub(crate) fn close_settings_dialog(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.settings_open = false;
        cx.notify();
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
                bg={rgb(crate::theme::CIPHER_BACKGROUND)}
                border_r_1 borderColor={rgb(crate::theme::CIPHER_BORDER)}
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
        .text_color(rgb(crate::theme::CIPHER_FOREGROUND_MUTED))
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
        crate::theme::CIPHER_FOREGROUND_SECONDARY
    } else {
        crate::theme::CIPHER_FOREGROUND_MUTED
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
    cx: &mut Context<Nox>,
) -> AnyElement {
    let resting_bg = if active {
        crate::theme::CIPHER_SURFACE_RAISED
    } else {
        crate::theme::CIPHER_BACKGROUND
    };
    let foreground = if active {
        crate::theme::CIPHER_FOREGROUND_SECONDARY
    } else {
        crate::theme::CIPHER_FOREGROUND_MUTED
    };
    // The user asked for hover to look exactly like the active state, so the
    // hover/active colors are the active background regardless of `active`.
    let variant = ButtonCustomVariant::new(cx)
        .color(rgb(resting_bg).into())
        .hover(rgb(crate::theme::CIPHER_SURFACE_RAISED).into())
        .active(rgb(crate::theme::CIPHER_SURFACE_RAISED).into())
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
                .text_color(rgb(crate::theme::CIPHER_FOREGROUND_MUTED))
                .bg(rgb(crate::theme::CIPHER_SURFACE_RAISED))
                .rounded(px(4.))
                .px(px(6.))
                .py(px(2.))
                .child("Soon"),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::ActiveView;
    use crate::assets::Assets;
    use gpui::AssetSource;
    use nox_core::ItemType;

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
                let path = source[index + 1..]
                    .split('"')
                    .next()
                    .expect("closing quote");
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
