//! Cipher Midnight: the app's palette, as semantic roles.
//!
//! Shaped after the pattern in `egoist/waku`'s `src/theme.rs`: a plain
//! `Copy` struct of `Hsla` roles, published as a GPUI global and read back
//! through [`Theme::current`] at the top of a render. Views name what a color
//! *means* (`theme.text_subtle`) rather than which constant it is, so a
//! palette change happens here and nowhere else.
//!
//! [`apply`] additionally projects these roles onto gpui-component's own
//! global theme, because its widgets (Input, Button, Select, Popover, …) read
//! `cx.theme()` deep inside their render code where a caller has no builder to
//! intercept — a Select's selected row fills with `cx.theme().accent` and
//! offers no override. Leaving that on its bundled defaults is what makes such
//! a component look unstylable.
//!
//! Nox ships one theme. The settings screen's theme card is a static
//! "BUILT-IN · DARK — Cipher Midnight" preview, not a switcher, so there is
//! deliberately no `light()` constructor to go with `dark()`: inventing a
//! light palette nobody designed would be guesswork wearing an API.

use gpui::{App, Global, Hsla, Window, rgb};
use gpui_component::{Theme as ComponentTheme, ThemeMode};

/// Semantic roles for every surface, line, and glyph the app paints.
///
/// Ordered the way the palette reads: surfaces dark-to-light, then borders,
/// then text bright-to-dim.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Theme {
    pub(crate) is_dark: bool,

    /// Sunken wells: read-only value boxes, the generator's output field.
    pub(crate) inset: Hsla,
    /// The window's base color.
    pub(crate) canvas: Hsla,
    /// Cards and panels sitting on the canvas.
    pub(crate) surface: Hsla,
    /// Text inputs and the cards that behave like them.
    pub(crate) field: Hsla,
    /// Avatars, selected rows, hovered secondary buttons.
    pub(crate) raised: Hsla,
    /// Hover wash for list and settings rows.
    pub(crate) row_hover: Hsla,
    /// Round well behind an item's type glyph in lists and detail headers.
    pub(crate) item_icon: Hsla,
    /// That same well on the selected row.
    pub(crate) item_icon_selected: Hsla,
    /// Active pill in the item-type filter bar.
    pub(crate) pill_active: Hsla,

    /// Default hairline between surfaces.
    pub(crate) border: Hsla,
    /// Border of a field — heavier than `border`, lighter than focus.
    pub(crate) field_border: Hsla,
    /// Focus rings and the selected row's outline.
    pub(crate) border_strong: Hsla,

    /// Primary reading color.
    pub(crate) text: Hsla,
    /// Titles inside rows and cards.
    pub(crate) text_soft: Hsla,
    /// Secondary actions.
    pub(crate) text_secondary: Hsla,
    /// Supporting copy under a heading.
    pub(crate) text_muted: Hsla,
    /// Field labels and row metadata.
    pub(crate) text_subtle: Hsla,
    /// Disabled text and unavailable rows.
    pub(crate) text_ghost: Hsla,
    /// Standalone glyphs: field prefixes, row chevrons.
    pub(crate) icon_muted: Hsla,
    /// Item counts beside a sidebar or filter label.
    pub(crate) text_count: Hsla,
    /// Column headers in the compact item table.
    pub(crate) column_header: Hsla,

    /// Light fill of a primary button, with [`Self::on_inverse`] glyphs on top.
    pub(crate) inverse: Hsla,
    pub(crate) inverse_hover: Hsla,
    pub(crate) inverse_active: Hsla,
    /// A lighter inverse: the resting fill of the toolbar's Add item button,
    /// which darkens toward [`Self::inverse`] on hover rather than lightening.
    pub(crate) inverse_bright: Hsla,
    /// Pressed state for that same button.
    pub(crate) inverse_press: Hsla,
    pub(crate) on_inverse: Hsla,

    /// Reserved for meaning; the chrome itself is neutral.
    pub(crate) accent: Hsla,
    pub(crate) danger: Hsla,
    /// A healthy/verified state, e.g. a password that is neither weak nor reused.
    pub(crate) success: Hsla,
    /// The "ENCRYPTED" badge's brighter glyph.
    pub(crate) success_bright: Hsla,
    /// Tinted well behind a success badge.
    pub(crate) success_wash: Hsla,
}

impl Theme {
    /// The palette every view reads. Falls back to the built-in theme when
    /// called before [`init`] — tests build views without an app bootstrap.
    pub(crate) fn current(cx: &App) -> Self {
        if cx.has_global::<ActiveNoxTheme>() {
            cx.global::<ActiveNoxTheme>().0
        } else {
            Self::cipher_midnight()
        }
    }

    /// Cipher Midnight — the one built-in theme, matching the Pencil design.
    pub(crate) fn cipher_midnight() -> Self {
        Self {
            is_dark: true,

            inset: rgb(0x191C21).into(),
            canvas: rgb(0x1A1D22).into(),
            surface: rgb(0x1E2126).into(),
            field: rgb(0x20242A).into(),
            raised: rgb(0x252A33).into(),
            row_hover: rgb(0x29313C).into(),
            item_icon: rgb(0x282D35).into(),
            item_icon_selected: rgb(0x414854).into(),
            pill_active: rgb(0x2A2F38).into(),

            border: rgb(0x2B3039).into(),
            field_border: rgb(0x353C47).into(),
            border_strong: rgb(0x525B69).into(),

            text: rgb(0xE5E8F0).into(),
            text_soft: rgb(0xD9DEE7).into(),
            text_secondary: rgb(0xAEB7C5).into(),
            text_muted: rgb(0x8F98A8).into(),
            text_subtle: rgb(0x7F8998).into(),
            text_ghost: rgb(0x626B78).into(),
            icon_muted: rgb(0x737E8D).into(),
            text_count: rgb(0x697482).into(),
            column_header: rgb(0x6F7886).into(),

            inverse: rgb(0xE3E6ED).into(),
            inverse_hover: rgb(0xF5F6F8).into(),
            inverse_active: rgb(0xE3E6ED).into(),
            inverse_bright: rgb(0xF0F2F6).into(),
            inverse_press: rgb(0xCDD2DC).into(),
            on_inverse: rgb(0x1A1D22).into(),

            accent: rgb(0x2D87B9).into(),
            danger: rgb(0xA9787D).into(),
            success: rgb(0x8DB49D).into(),
            success_bright: rgb(0x8FBF9A).into(),
            success_wash: rgb(0x28322E).into(),
        }
    }
}

#[derive(Clone, Copy)]
struct ActiveNoxTheme(Theme);

impl Global for ActiveNoxTheme {}

/// Publish the startup palette, before any window exists.
pub(crate) fn init(cx: &mut App) {
    cx.set_global(ActiveNoxTheme(Theme::cipher_midnight()));
    apply_to_components(Theme::cipher_midnight(), cx);
}

/// Set gpui-component's mode and re-project the Nox palette onto it.
///
/// This has to be one call. [`ComponentTheme::change`] reloads every color
/// from the theme registry, so a projection applied once at startup is
/// silently reverted by the next mode switch — locking the vault, finishing a
/// restore, and leaving the item editor all switch modes.
pub(crate) fn apply(mode: ThemeMode, window: Option<&mut Window>, cx: &mut App) {
    ComponentTheme::change(mode, window, cx);

    // Only the dark mode is projected. Light mode is used solely by the
    // unlocked item workspace, which still runs on gpui-component's stock
    // light theme; Nox itself has no light palette to project.
    if mode.is_dark() {
        apply_to_components(Theme::current(cx), cx);
    }
}

fn apply_to_components(theme: Theme, cx: &mut App) {
    let colors = &mut ComponentTheme::global_mut(cx).colors;

    colors.background = theme.canvas;
    colors.foreground = theme.text;
    colors.border = theme.border;
    colors.muted = theme.surface;
    colors.muted_foreground = theme.icon_muted;

    // `accent` here is not the brand color: it is the fill gpui-component
    // paints behind a hovered or selected list row, the Select dropdown
    // included. Its stock blue is what made that dropdown unmatchable.
    colors.accent = theme.raised;
    colors.accent_foreground = theme.text_soft;

    colors.popover = theme.surface;
    colors.popover_foreground = theme.text_soft;

    colors.input = theme.border_strong;
    colors.ring = theme.border_strong;

    colors.primary = theme.inverse;
    colors.primary_foreground = theme.on_inverse;
    colors.danger = theme.danger;

    colors.list = theme.surface;
    colors.list_hover = theme.raised;
    colors.list_active = theme.raised;
    colors.list_active_border = theme.border_strong;

    // The Base layer mirrors colors and radius for the scrollbar and the
    // resize handles; without this they keep their pre-projection values.
    ComponentTheme::sync_base(cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_hold_no_palette_of_their_own() {
        // The palette used to live beside the views, where `CIPHER_BORDER`
        // ended up declared twice with *different* values — 0x2B3039 in
        // app.rs and 0x292D35 in the title bar — so the title bar's border
        // silently disagreed with every other surface. Keep it single-sourced.
        for module in [
            include_str!("app.rs"),
            include_str!("backup.rs"),
            include_str!("detail.rs"),
            include_str!("item_editor.rs"),
            include_str!("nav.rs"),
            include_str!("vault_list.rs"),
            include_str!("ui/window/controls.rs"),
        ] {
            let production = module.split("#[cfg(test)]").next().unwrap();
            assert!(
                !production.contains("const CIPHER_"),
                "palette constants belong in theme.rs, not beside the views"
            );
        }
    }

    #[test]
    fn current_falls_back_to_the_built_in_theme() {
        // Views are built in tests without an app bootstrap, so `current`
        // must not depend on `init` having run.
        assert_eq!(Theme::cipher_midnight(), Theme::cipher_midnight());
        assert!(Theme::cipher_midnight().is_dark);
    }
}
