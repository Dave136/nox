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
    "window-maximize",
    "window-minimize",
    "window-restore",
    "x",
    "search"
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IconName {
    Search,
    WindowMaximize,
    WindowMinimize,
    WindowRestore,
    X,
}

fn get_icon_name(name: &IconName) -> &'static str {
    match name {
        IconName::Search => "search.svg",
        IconName::WindowMaximize => "window-maximize.svg",
        IconName::WindowMinimize => "window-minimize.svg",
        IconName::WindowRestore => "window-restore.svg",
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
        .text_color(color.unwrap_or(hsla(0., 0., 0., 1.)))
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(ICONS
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| SharedString::from(*name))
            .collect())
    }
}
