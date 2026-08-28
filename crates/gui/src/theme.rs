//! The Cipher palette and its projection onto gpui-component's theme.
//!
//! Two things share this file on purpose:
//!
//! 1. The `CIPHER_*` tokens the app's own `div()` trees paint with.
//! 2. [`apply`], which pushes those same tokens into gpui-component's global
//!    [`Theme`] so its components (Input, Button, Select, Popover, …) paint
//!    from the same palette instead of their bundled defaults.
//!
//! Keeping (2) next to (1) is the point. gpui-component's widgets read
//! `cx.theme()` deep inside their own render code, where a caller has no
//! builder to intercept — a Select's selected row, for one, fills with
//! `cx.theme().accent` and offers no override. Styling those components from
//! the outside while leaving `cx.theme()` on its defaults is what makes a
//! component "unstylable"; the fix is to configure the theme once, here.

use gpui::{App, Hsla, Window, rgb};
use gpui_component::{Theme, ThemeMode};

pub(crate) const CIPHER_BACKGROUND: u32 = 0x1A1D22;
pub(crate) const CIPHER_SURFACE: u32 = 0x1E2126;
pub(crate) const CIPHER_SURFACE_RAISED: u32 = 0x252A33;
pub(crate) const CIPHER_BORDER: u32 = 0x2B3039;
pub(crate) const CIPHER_BORDER_STRONG: u32 = 0x525B69;
pub(crate) const CIPHER_FOREGROUND: u32 = 0xE5E8F0;
pub(crate) const CIPHER_FOREGROUND_SOFT: u32 = 0xD9DEE7;
pub(crate) const CIPHER_FOREGROUND_SECONDARY: u32 = 0xAEB7C5;
pub(crate) const CIPHER_FOREGROUND_MUTED: u32 = 0x8F98A8;
pub(crate) const CIPHER_FOREGROUND_SUBTLE: u32 = 0x7F8998;
pub(crate) const CIPHER_DISABLED: u32 = 0x626B78;
pub(crate) const CIPHER_PRIMARY: u32 = 0xE3E6ED;
pub(crate) const CIPHER_PRIMARY_AUTH: u32 = CIPHER_PRIMARY;
pub(crate) const CIPHER_PRIMARY_AUTH_HOVER: u32 = 0xF5F6F8;
pub(crate) const CIPHER_PRIMARY_AUTH_ACTIVE: u32 = CIPHER_PRIMARY;
pub(crate) const CIPHER_DANGER: u32 = 0xA9787D;
pub(crate) const CIPHER_ICON_MUTED: u32 = 0x737E8D;
pub(crate) const CIPHER_PRIMARY_FOREGROUND: u32 = 0x1A1D22;
/// Mirrors the Pencil frame's `--cipher-accent-blue` token, which the design
/// file itself doesn't currently render onscreen (an inert key-badge layer
/// behind the header logo). Kept for parity with the design's token set.
#[allow(dead_code)]
pub(crate) const CIPHER_ACCENT_BLUE: u32 = 0x2D87B9;

fn hsla(color: u32) -> Hsla {
    rgb(color).into()
}

/// Set the theme mode and repaint gpui-component's palette in Cipher colors.
///
/// Call this instead of [`Theme::change`] everywhere. `Theme::change` reloads
/// every color from the theme registry, so a customization applied once at
/// startup is silently reverted by the next mode switch — locking the vault,
/// finishing a restore, and returning from the item editor all switch modes.
/// Wrapping both steps in one call is what keeps that from drifting.
pub(crate) fn apply(mode: ThemeMode, window: Option<&mut Window>, cx: &mut App) {
    Theme::change(mode, window, cx);

    // Only the dark palette is themed: every auth and vault surface is dark by
    // design, and the light mode is used solely by the unlocked item views,
    // which are still on gpui-component's stock light theme.
    if !mode.is_dark() {
        return;
    }

    let theme = Theme::global_mut(cx);
    let colors = &mut theme.colors;

    colors.background = hsla(CIPHER_BACKGROUND);
    colors.foreground = hsla(CIPHER_FOREGROUND);
    colors.border = hsla(CIPHER_BORDER);
    colors.muted = hsla(CIPHER_SURFACE);
    colors.muted_foreground = hsla(CIPHER_ICON_MUTED);

    // `accent` is the one that bit us: it is the fill gpui-component paints
    // behind a hovered/selected list row (Select's dropdown included), and it
    // defaults to a blue that has nothing to do with this palette.
    colors.accent = hsla(CIPHER_SURFACE_RAISED);
    colors.accent_foreground = hsla(CIPHER_FOREGROUND_SOFT);

    colors.popover = hsla(CIPHER_SURFACE);
    colors.popover_foreground = hsla(CIPHER_FOREGROUND_SOFT);

    colors.input = hsla(CIPHER_BORDER_STRONG);
    colors.ring = hsla(CIPHER_BORDER_STRONG);

    colors.primary = hsla(CIPHER_PRIMARY);
    colors.primary_foreground = hsla(CIPHER_PRIMARY_FOREGROUND);
    colors.danger = hsla(CIPHER_DANGER);

    colors.list = hsla(CIPHER_SURFACE);
    colors.list_hover = hsla(CIPHER_SURFACE_RAISED);
    colors.list_active = hsla(CIPHER_SURFACE_RAISED);
    colors.list_active_border = hsla(CIPHER_BORDER_STRONG);

    // The Base layer mirrors colors/radius for the scrollbar and resize
    // handles; without this they keep the pre-override values.
    Theme::sync_base(cx);
}

#[cfg(test)]
mod tests {

    #[test]
    fn cipher_border_has_one_definition() {
        // `CIPHER_BORDER` was previously declared twice with *different*
        // values — 0x2B3039 in app.rs and 0x292D35 in the title bar — so the
        // title bar's border silently disagreed with every other surface.
        // Keep the palette single-sourced here.
        // Scan only the production half — this test's own assertions mention
        // the constant by name and would otherwise count themselves.
        let source = include_str!("theme.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap();
        assert_eq!(source.matches("const CIPHER_BORDER:").count(), 1);
        for module in [
            include_str!("app.rs"),
            include_str!("ui/window/controls.rs"),
        ] {
            let production = module.split("#[cfg(test)]").next().unwrap();
            assert!(
                !production.contains("const CIPHER_"),
                "palette constants belong in theme.rs, not beside the views"
            );
        }
    }
}
