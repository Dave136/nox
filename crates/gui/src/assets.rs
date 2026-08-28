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

const DEFAULT_FONT_SIZE: f32 = 14.0;

const ICONS: &[(&str, &[u8])] = icons![
    "archive-restore",
    "arrow-left-right",
    "arrow-up-down",
    "bell",
    "bold",
    "calendar-days",
    "check",
    "circle-user-around",
    "clock-3",
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
    "globe",
    "house",
    "italic",
    "key-round",
    "key-square",
    "keyboard",
    "layout-grid",
    "lightbulb",
    "list",
    "list-filter",
    "lock",
    "lock-keyhole",
    "lock-keyhole-open",
    "mouse-pointer-2",
    "notebook-pen",
    "nox-logo",
    "oalette",
    "pencil",
    "pencil-sparkles",
    "plus",
    "refresh-cw",
    "scan-eye",
    "search",
    "settings",
    "shield-alert",
    "shield-check",
    "shield-plus",
    "star",
    "tag",
    "trash-2",
    "user-round",
    "user-round-plus",
    "wand-sparkles",
    "window-maximize",
    "window-minimize",
    "window-restore",
    "x"
];

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
