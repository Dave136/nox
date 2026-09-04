//! Icon choices, private local-image storage, and the shared render resolver.
//!
//! Bundled presets and the type defaults are synced by `nox-core` as a small
//! `IconChoice`. Fetched and uploaded bytes never enter an item payload: they
//! are stored under this device's private cache and referenced only by GUI
//! state.

use gpui::{AnyElement, Hsla, IntoElement, ParentElement, Styled, div, px};
use nox_core::{IconChoice, ItemPayload, ItemType, PresetIcon};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) const MAX_LOCAL_IMAGE_BYTES: usize = 5 * 1024 * 1024;
const SELECTIONS_FILE: &str = "selections";
static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LocalIconRef {
    pub(crate) item_key: String,
    pub(crate) content_hash: String,
    pub(crate) cache_path: PathBuf,
}

#[derive(Debug)]
pub(crate) enum LocalIconError {
    TooLarge,
    InvalidImage,
    Io(io::Error),
}

impl std::fmt::Display for LocalIconError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => write!(f, "Image files must be 5 MiB or smaller."),
            Self::InvalidImage => {
                write!(f, "Choose a valid PNG, JPEG, GIF, WebP, ICO, or SVG image.")
            }
            Self::Io(error) => write!(f, "Could not store the image locally: {error}"),
        }
    }
}

impl std::error::Error for LocalIconError {}

impl From<io::Error> for LocalIconError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub(crate) enum ResolvedIcon {
    /// A bundled icon path, rendered through the existing SVG asset loader.
    Svg(&'static str),
    /// A selected favicon or uploaded-image cache file.
    LocalImage(PathBuf),
    /// The selected local image is unavailable on this device.
    UnavailableLocalImage,
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

pub(crate) fn is_image_content_type(content_type: &str) -> bool {
    let content_type = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim()
        .to_ascii_lowercase();
    matches!(
        content_type.as_str(),
        "image/png"
            | "image/jpeg"
            | "image/gif"
            | "image/webp"
            | "image/svg+xml"
            | "image/x-icon"
            | "image/vnd.microsoft.icon"
    )
}

pub(crate) fn resolve_item_icon(
    item_type: ItemType,
    icon: IconChoice,
    _data_dir: &Path,
    local_item_key: &str,
    local_selection: Option<&LocalIconRef>,
) -> ResolvedIcon {
    if let Some(local) = local_selection.filter(|local| local.item_key == local_item_key) {
        return if local.cache_path.is_file() {
            ResolvedIcon::LocalImage(local.cache_path.clone())
        } else {
            ResolvedIcon::UnavailableLocalImage
        };
    }
    match icon {
        IconChoice::Default => ResolvedIcon::Svg(default_type_icon(item_type)),
        IconChoice::Preset(preset) => ResolvedIcon::Svg(preset_icon_path(preset)),
        // A synced favicon choice has no bytes on a new device. Keep that choice
        // explicit rather than silently changing it to the type default.
        IconChoice::Favicon => ResolvedIcon::UnavailableLocalImage,
    }
}

pub(crate) fn resolved_item_icon(
    data_dir: &Path,
    item_key: &str,
    item: &ItemPayload,
    local_selection: Option<&LocalIconRef>,
) -> ResolvedIcon {
    resolve_item_icon(
        item.item_type,
        item.icon,
        data_dir,
        item_key,
        local_selection,
    )
}

/// Render every resolver variant. The unavailable branch is intentionally
/// visible, so a missing selected image can be retried/replaced instead of
/// looking like the type default.
pub(crate) fn render_resolved_icon(resolved: ResolvedIcon, size: f32, color: Hsla) -> AnyElement {
    match resolved {
        ResolvedIcon::Svg(path) => gpui_component::Icon::empty()
            .path(path)
            .size(px(size))
            .text_color(color)
            .into_any_element(),
        ResolvedIcon::LocalImage(path) => gpui::img(path)
            .size(px(size))
            .rounded(px((size / 4.).max(2.)))
            .into_any_element(),
        ResolvedIcon::UnavailableLocalImage => div()
            .size(px(size))
            .flex()
            .items_center()
            .justify_center()
            .child(
                gpui_component::Icon::empty()
                    .path("icons/scan-eye.svg")
                    .size(px((size * 0.72).max(10.)))
                    .text_color(color),
            )
            .into_any_element(),
    }
}

pub(crate) fn item_key(item_id: nox_core::ItemId) -> String {
    format!("item:{}", hex_bytes(item_id.as_bytes()))
}

pub(crate) fn editor_key(editor_id: u64) -> String {
    format!("editor:{editor_id}")
}

fn cache_root(data_dir: &Path) -> PathBuf {
    data_dir.join("icon-cache")
}

fn cache_key_dir(data_dir: &Path, item_key: &str) -> PathBuf {
    cache_root(data_dir).join(hex_digest(item_key.as_bytes()))
}

fn selection_file(data_dir: &Path) -> PathBuf {
    cache_root(data_dir).join(SELECTIONS_FILE)
}

fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex_bytes(&digest)
}

fn hex_bytes(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(result, "{byte:02x}");
    }
    result
}

fn set_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    Ok(())
}

fn write_private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "cache path has no parent"))?;
    set_private_dir(parent)?;
    if path.is_file() {
        return Ok(());
    }
    let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(".tmp-{}-{sequence}", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    match fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(_) if path.is_file() => Ok(()),
        Err(error) => Err(error),
    }
}

fn image_signature_is_valid(bytes: &[u8]) -> bool {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return bytes.len() >= 24
            && &bytes[12..16] == b"IHDR"
            && bytes[16..20].iter().any(|byte| *byte != 0)
            && bytes[20..24].iter().any(|byte| *byte != 0);
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return bytes.len() >= 10 && bytes[6..10].iter().any(|byte| *byte != 0);
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return bytes.len() >= 4 && bytes.ends_with(b"\xff\xd9");
    }
    if bytes.len() >= 20 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return &bytes[12..16] != b"";
    }
    if bytes.len() >= 22 && bytes[..4] == [0, 0, 1, 0] {
        return u16::from_le_bytes([bytes[4], bytes[5]]) > 0;
    }
    let lower = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    lower.contains("<svg")
        && lower.contains('>')
        && !lower.contains("<script")
        && !lower.contains("foreignobject")
        && !lower.contains("javascript:")
}

pub(crate) fn validate_local_image(bytes: &[u8]) -> Result<(), LocalIconError> {
    if bytes.len() > MAX_LOCAL_IMAGE_BYTES {
        return Err(LocalIconError::TooLarge);
    }
    if image_signature_is_valid(bytes) {
        Ok(())
    } else {
        Err(LocalIconError::InvalidImage)
    }
}

pub(crate) fn cache_local_icon(
    data_dir: &Path,
    item_key: &str,
    bytes: &[u8],
) -> Result<LocalIconRef, LocalIconError> {
    if item_key.is_empty() {
        return Err(LocalIconError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty local icon key",
        )));
    }
    validate_local_image(bytes)?;
    let content_hash = hex_digest(bytes);
    let path = cache_key_dir(data_dir, item_key).join(&content_hash);
    write_private_file(&path, bytes)?;
    Ok(LocalIconRef {
        item_key: item_key.to_owned(),
        content_hash,
        cache_path: path,
    })
}

pub(crate) fn cache_local_icon_from_path(
    data_dir: &Path,
    item_key: &str,
    source: &Path,
) -> Result<LocalIconRef, LocalIconError> {
    let file = File::open(source)?;
    let metadata = file.metadata()?;
    if metadata.len() > MAX_LOCAL_IMAGE_BYTES as u64 {
        return Err(LocalIconError::TooLarge);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_LOCAL_IMAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    cache_local_icon(data_dir, item_key, &bytes)
}

pub(crate) fn load_local_selections(data_dir: &Path) -> HashMap<String, LocalIconRef> {
    let path = selection_file(data_dir);
    let Ok(contents) = fs::read_to_string(path) else {
        return HashMap::new();
    };
    let mut selections = HashMap::new();
    for line in contents.lines() {
        let Some((item_key, content_hash)) = line.split_once('\t') else {
            continue;
        };
        if item_key.is_empty()
            || content_hash.len() != 64
            || !content_hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            continue;
        }
        let cache_path = cache_key_dir(data_dir, item_key).join(content_hash);
        selections.insert(
            item_key.to_owned(),
            LocalIconRef {
                item_key: item_key.to_owned(),
                content_hash: content_hash.to_owned(),
                cache_path,
            },
        );
    }
    selections
}

pub(crate) fn persist_local_selections(
    data_dir: &Path,
    selections: &HashMap<String, LocalIconRef>,
) -> io::Result<()> {
    let mut keys = selections.keys().collect::<Vec<_>>();
    keys.sort();
    let mut contents = String::new();
    for key in keys {
        let Some(selection) = selections.get(key) else {
            continue;
        };
        contents.push_str(key);
        contents.push('\t');
        contents.push_str(&selection.content_hash);
        contents.push('\n');
    }
    write_private_file(&selection_file(data_dir), contents.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "nox-icons-{label}-{}-{}",
            std::process::id(),
            NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    fn valid_png() -> Vec<u8> {
        vec![
            0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, b'I', b'H', b'D', b'R', 0,
            0, 0, 1, 0, 0, 0, 1,
        ]
    }

    #[test]
    fn default_resolves_to_the_item_types_own_icon() {
        assert!(matches!(
            resolve_item_icon(
                ItemType::Login,
                IconChoice::Default,
                Path::new("/nope"),
                "item",
                None
            ),
            ResolvedIcon::Svg("icons/key-square.svg")
        ));
        assert!(matches!(
            resolve_item_icon(
                ItemType::SecureNote,
                IconChoice::Default,
                Path::new("/nope"),
                "item",
                None
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
                "item",
                None,
            ),
            ResolvedIcon::Svg("icons/briefcase.svg")
        ));
    }

    #[test]
    fn selected_local_image_resolves_without_falling_back() {
        let dir = temp_dir("selected");
        let selection = cache_local_icon(&dir, "item-key", &valid_png()).unwrap();
        let resolved = resolve_item_icon(
            ItemType::Login,
            IconChoice::Default,
            &dir,
            "item-key",
            Some(&selection),
        );
        assert!(matches!(resolved, ResolvedIcon::LocalImage(path) if path == selection.cache_path));
    }

    #[test]
    fn missing_local_image_is_explicitly_unavailable() {
        let dir = temp_dir("missing");
        let selection = LocalIconRef {
            item_key: "item-key".to_owned(),
            content_hash: "a".repeat(64),
            cache_path: dir.join("missing"),
        };
        assert!(matches!(
            resolve_item_icon(
                ItemType::Login,
                IconChoice::Default,
                &dir,
                "item-key",
                Some(&selection)
            ),
            ResolvedIcon::UnavailableLocalImage
        ));
    }

    #[test]
    fn synced_favicon_without_local_selection_is_explicitly_unavailable() {
        assert!(matches!(
            resolve_item_icon(
                ItemType::Login,
                IconChoice::Favicon,
                Path::new("/nope"),
                "item",
                None
            ),
            ResolvedIcon::UnavailableLocalImage
        ));
    }

    #[test]
    fn local_cache_is_private_content_addressed_and_metadata_has_no_source_path() {
        let dir = temp_dir("cache");
        let selection = cache_local_icon(&dir, "editor:4", &valid_png()).unwrap();
        assert!(selection.cache_path.starts_with(dir.join("icon-cache")));
        assert!(selection.cache_path.ends_with(&selection.content_hash));
        let mut selections = HashMap::new();
        selections.insert(selection.item_key.clone(), selection.clone());
        persist_local_selections(&dir, &selections).unwrap();
        let loaded = load_local_selections(&dir);
        assert_eq!(loaded.get("editor:4"), Some(&selection));
        let metadata = fs::read_to_string(selection_file(&dir)).unwrap();
        assert!(!metadata.contains(dir.to_string_lossy().as_ref()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(cache_root(&dir)).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(&selection.cache_path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(selection_file(&dir))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn malformed_image_bytes_are_rejected_before_cache() {
        let dir = temp_dir("invalid");
        assert!(matches!(
            cache_local_icon(&dir, "item", b"<html>not an image</html>"),
            Err(LocalIconError::InvalidImage)
        ));
    }
}
