use std::borrow::Cow;

use anyhow::Result;
use gpui::{AssetSource, Hsla, SharedString, Styled, Svg, hsla, rems, svg};

/// Icons embedded in the binary so the app stays a single artifact.
pub struct Assets;

macro_rules! icons {
    ($($name:literal),+ $(,)?) => {
        &[$((
            concat!("icons/", $name, ".svg"),
            include_bytes!(concat!("./assets/icons/", $name, ".svg")).as_slice(),
        )),+]
    };
}

macro_rules! fonts {
    ($($name:literal),+ $(,)?) => {
        &[$((
            concat!("fonts/", $name, ".ttf"),
            include_bytes!(concat!("./assets/fonts/", $name, ".ttf")).as_slice(),
        )),+]
    };
}

const DEFAULT_FONT_SIZE: f32 = 14.0;

const ICONS: &[(&str, &[u8])] = icons![
    "archive-restore",
    "arrow-left-right",
    "arrow-up-down",
    "bell",
    "bold",
    "briefcase",
    "calendar-days",
    "check",
    "circle-user-around",
    "clapperboard",
    "clock-3",
    "cloud",
    "cloud-check",
    "code",
    "contact",
    "copy-plus",
    "credit-card",
    "database",
    "ellipsis",
    "ellipsis-vertical",
    "external-link",
    "eye-off",
    "file-lock",
    "file-sliders",
    "folder-closed",
    "gamepad-2",
    "globe",
    "graduation-cap",
    "heart-pulse",
    "house",
    "image-off",
    "image-plus",
    "image-up",
    "italic",
    "key-round",
    "key-square",
    "keyboard",
    "landmark",
    "link",
    "layout-grid",
    "lightbulb",
    "list",
    "list-filter",
    "lock",
    "lock-keyhole",
    "lock-keyhole-open",
    "mail",
    "messages-square",
    "mouse-pointer-2",
    "music",
    "notebook-pen",
    "nox-logo",
    "oalette",
    "pencil",
    "pencil-sparkles",
    "plane",
    "plus",
    "refresh-cw",
    "rotate-ccw",
    "scan-eye",
    "search",
    "settings",
    "shield-alert",
    "shield-check",
    "shield-plus",
    "shopping-cart",
    "sliders-horizontal",
    "star",
    "store",
    "tag",
    "trash-2",
    "upload",
    "user-round",
    "user-round-plus",
    "wand-sparkles",
    "window-maximize",
    "window-minimize",
    "window-restore",
    "x"
];

/// The UI typeface, embedded for the same reason the icons are — except here
/// it is a correctness requirement, not just packaging tidiness.
///
/// GPUI resolves a family by *exact name*: `load_family` keeps only faces whose
/// family matches, and `find_best_match` then picks the weight/style among
/// those. When the name resolves to nothing (`.SystemUIFont` becomes
/// "IBM Plex Sans" on Linux, which most distros do not ship), `resolve_font`
/// falls through to its own fallback stack and passes *that* fallback's `Font`
/// — built with the default weight and upright style. Every `font_weight(..)`
/// and `italic()` in the app is silently discarded at that point, and GPUI
/// never synthesizes either one. Shipping the faces is what makes bold and
/// italic render at all, on any machine.
///
/// Keep an upright *and* an italic face for every weight the UI asks for:
/// dropping one does not fail the build, it just silently stops rendering.
const FONTS: &[(&str, &[u8])] = fonts![
    "Geist-Regular",
    "Geist-Italic",
    "Geist-Medium",
    "Geist-SemiBold",
    "Geist-Bold",
    "Geist-BoldItalic",
];

/// The embedded faces, ready for `TextSystem::add_fonts` at startup.
pub fn fonts() -> Vec<Cow<'static, [u8]>> {
    FONTS
        .iter()
        .map(|(_, bytes)| Cow::Borrowed(*bytes))
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IconName {
    Search,
    WindowMaximize,
    WindowMinimize,
    X,
}

fn get_icon_name(name: &IconName) -> &'static str {
    match name {
        IconName::Search => "search.svg",
        IconName::WindowMaximize => "window-maximize.svg",
        IconName::WindowMinimize => "window-minimize.svg",
        IconName::X => "x.svg",
    }
}

pub fn icon(name: IconName, size: Option<f32>, color: Option<Hsla>) -> Svg {
    let icon_name = get_icon_name(&name);

    let icon_path = format!("icons/{}", icon_name);

    svg()
        .path(icon_path)
        .w(size
            .map(|s| rems(s / DEFAULT_FONT_SIZE))
            .unwrap_or(rems(16.0 / DEFAULT_FONT_SIZE)))
        .h(size
            .map(|s| rems(s / DEFAULT_FONT_SIZE))
            .unwrap_or(rems(16.0 / DEFAULT_FONT_SIZE)))
        .text_color(color.map_or(hsla(0., 0., 0., 1.), |c| c))
}

pub fn logo(size: f32, color: Hsla) -> Svg {
    svg()
        .path("icons/nox-logo.svg")
        .size(rems(size / DEFAULT_FONT_SIZE))
        .text_color(color)
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = ICONS.iter().find(|(name, _)| *name == path) {
            return Ok(Some(Cow::Borrowed(*bytes)));
        }
        // Fall back to gpui-component's bundled icon set (settings, copy,
        // check, book-open, ...) so components like Sidebar/Sheet/Button
        // that reference `gpui_component::IconName` resolve too.
        gpui_component_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut names: Vec<SharedString> = ICONS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(*name))
            .collect();
        names.extend(gpui_component_assets::Assets.list(path)?);
        Ok(names)
    }
}

#[cfg(test)]
mod tests {
    use super::{FONTS, ICONS, fonts};

    // Font *resolution* is deliberately not asserted here: `TestPlatform` runs
    // on `NoopTextSystem`, which hands back one FontId for every request, so a
    // test resolving Regular/Bold/Italic through it reports them identical
    // whether or not the faces are registered. It measures the stub, not the
    // app. What is checkable without a real text system is that the faces
    // ship — which is the part that actually goes missing.

    /// Bold and italic render only while an actual face carries that weight and
    /// slant — GPUI synthesizes neither, and drops the request without an error
    /// when no face matches. Losing a face here is therefore invisible at build
    /// time and shows up as flat text in the UI, so pin the set.
    #[test]
    fn embedded_fonts_cover_every_weight_and_slant_the_ui_asks_for() {
        for face in [
            "fonts/Geist-Regular.ttf",
            "fonts/Geist-Italic.ttf",
            "fonts/Geist-Medium.ttf",
            "fonts/Geist-SemiBold.ttf",
            "fonts/Geist-Bold.ttf",
            "fonts/Geist-BoldItalic.ttf",
        ] {
            let (_, bytes) = FONTS
                .iter()
                .find(|(name, _)| *name == face)
                .unwrap_or_else(|| panic!("{face} is no longer embedded"));
            // TrueType outlines: version 1.0 sfnt tag. Catches a face replaced
            // by an OTF/WOFF or by a truncated copy.
            assert_eq!(
                &bytes[..4],
                &[0x00, 0x01, 0x00, 0x00],
                "{face} is not a TrueType font"
            );
        }
        assert_eq!(fonts().len(), FONTS.len());
    }

    #[test]
    fn embedded_icon_svgs_do_not_contain_vue_scoped_attributes() {
        for (path, bytes) in ICONS {
            let svg = std::str::from_utf8(bytes).expect("embedded icon SVG should be UTF-8");
            assert!(
                !svg.contains("data-v-"),
                "{path} contains a Vue scoped data-v attribute that GPUI does not render"
            );
        }
    }
}
