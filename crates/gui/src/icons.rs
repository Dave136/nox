//! Resolves an item's chosen icon to something renderable, and owns the
//! `PresetIcon` <-> bundled-SVG-path/label mapping. `nox_core` only names
//! the choice (`IconChoice`/`PresetIcon`); rendering is a `gui` concern.

use crate::favicon::favicon_cache_path;
use nox_core::{IconChoice, ItemType, PresetIcon};
use std::path::{Path, PathBuf};

pub(crate) enum ResolvedIcon {
    /// A bundled icon path, as `gpui_component::Icon::empty().path(...)` already takes.
    Svg(&'static str),
    /// An on-disk favicon cache file to render as an image.
    Favicon(PathBuf),
}

pub(crate) fn default_type_icon(item_type: ItemType) -> &'static str {
    match item_type {
        ItemType::Login => "icons/key-square.svg",
        ItemType::SecureNote => "icons/file-lock.svg",
    }
}

pub(crate) fn preset_icon_path(preset: PresetIcon) -> &'static str {
    use PresetIcon::*;
    match preset {
        Globe => "icons/globe.svg",
        House => "icons/house.svg",
        CreditCard => "icons/credit-card.svg",
        Database => "icons/database.svg",
        Code => "icons/code.svg",
        Contact => "icons/contact.svg",
        ShieldCheck => "icons/shield-check.svg",
        Landmark => "icons/landmark.svg",
        ShoppingCart => "icons/shopping-cart.svg",
        Store => "icons/store.svg",
        Mail => "icons/mail.svg",
        MessagesSquare => "icons/messages-square.svg",
        Briefcase => "icons/briefcase.svg",
        Gamepad2 => "icons/gamepad-2.svg",
        Music => "icons/music.svg",
        Clapperboard => "icons/clapperboard.svg",
        Cloud => "icons/cloud.svg",
        HeartPulse => "icons/heart-pulse.svg",
        Plane => "icons/plane.svg",
        GraduationCap => "icons/graduation-cap.svg",
    }
}

pub(crate) fn preset_icon_label(preset: PresetIcon) -> &'static str {
    use PresetIcon::*;
    match preset {
        Globe => "Globe",
        House => "House",
        CreditCard => "Credit card",
        Database => "Database",
        Code => "Code",
        Contact => "Contact",
        ShieldCheck => "Shield",
        Landmark => "Bank",
        ShoppingCart => "Shopping",
        Store => "Store",
        Mail => "Mail",
        MessagesSquare => "Messages",
        Briefcase => "Work",
        Gamepad2 => "Gaming",
        Music => "Music",
        Clapperboard => "Video",
        Cloud => "Cloud",
        HeartPulse => "Health",
        Plane => "Travel",
        GraduationCap => "Education",
    }
}

pub(crate) const ALL_PRESETS: [PresetIcon; 20] = {
    use PresetIcon::*;
    [
        Globe,
        House,
        CreditCard,
        Database,
        Code,
        Contact,
        ShieldCheck,
        Landmark,
        ShoppingCart,
        Store,
        Mail,
        MessagesSquare,
        Briefcase,
        Gamepad2,
        Music,
        Clapperboard,
        Cloud,
        HeartPulse,
        Plane,
        GraduationCap,
    ]
};

pub(crate) fn resolve_item_icon(
    item_type: ItemType,
    icon: IconChoice,
    data_dir: &Path,
    uris: &[String],
) -> ResolvedIcon {
    match icon {
        IconChoice::Default => ResolvedIcon::Svg(default_type_icon(item_type)),
        IconChoice::Preset(preset) => ResolvedIcon::Svg(preset_icon_path(preset)),
        IconChoice::Favicon => uris
            .iter()
            .find_map(|uri| crate::favicon::extract_host(uri))
            .map(|host| favicon_cache_path(data_dir, &host))
            .filter(|path| path.is_file())
            .map_or_else(
                || ResolvedIcon::Svg(default_type_icon(item_type)),
                ResolvedIcon::Favicon,
            ),
    }
}

/// Shared by every per-item render site (list rows, detail panel, home's
/// recent-items list): resolves and, for a cache-miss favicon, falls back
/// to the type default — those spots are a fixed-size glyph well, same as
/// the picker trigger, not a place for a per-row image variant yet.
// ponytail: no caller yet — the next task wires this into vault_list.rs,
// detail.rs, and workspace.rs. Drop this allow once it does.
#[allow(dead_code)]
pub(crate) fn resolved_icon_path(data_dir: &Path, item: &nox_core::ItemPayload) -> &'static str {
    match resolve_item_icon(item.item_type, item.icon, data_dir, &item.uris) {
        ResolvedIcon::Svg(path) => path,
        ResolvedIcon::Favicon(_) => default_type_icon(item.item_type),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("nox-icons-test-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn default_resolves_to_the_item_types_own_icon() {
        assert!(matches!(
            resolve_item_icon(
                ItemType::Login,
                IconChoice::Default,
                Path::new("/nope"),
                &[]
            ),
            ResolvedIcon::Svg("icons/key-square.svg")
        ));
        assert!(matches!(
            resolve_item_icon(
                ItemType::SecureNote,
                IconChoice::Default,
                Path::new("/nope"),
                &[]
            ),
            ResolvedIcon::Svg("icons/file-lock.svg")
        ));
    }

    #[test]
    fn preset_resolves_to_its_own_svg_regardless_of_item_type() {
        assert!(matches!(
            resolve_item_icon(
                ItemType::SecureNote,
                IconChoice::Preset(PresetIcon::Briefcase),
                Path::new("/nope"),
                &[],
            ),
            ResolvedIcon::Svg("icons/briefcase.svg")
        ));
    }

    #[test]
    fn favicon_with_a_cache_hit_resolves_to_the_cache_file() {
        let dir = temp_dir("hit");
        crate::favicon::cache_favicon(&dir, "example.test", b"bytes").unwrap();
        let uris = vec!["https://example.test".to_owned()];
        let resolved = resolve_item_icon(ItemType::Login, IconChoice::Favicon, &dir, &uris);
        assert!(matches!(resolved, ResolvedIcon::Favicon(path) if path.is_file()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn favicon_with_a_cache_miss_falls_back_to_the_default_icon() {
        let dir = temp_dir("miss");
        let uris = vec!["https://example.test".to_owned()];
        let resolved = resolve_item_icon(ItemType::Login, IconChoice::Favicon, &dir, &uris);
        assert!(matches!(
            resolved,
            ResolvedIcon::Svg("icons/key-square.svg")
        ));
    }
}
