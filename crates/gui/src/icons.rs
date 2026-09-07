//! Icon choices, private local-image storage, and the shared render resolver.
//!
//! Bundled presets and the type defaults are synced by `nox-core` as a small
//! `IconChoice`. Fetched and uploaded bytes never enter an item payload: they
//! are stored under this device's private cache and referenced only by GUI
//! state.

use crate::theme::Theme;
use gpui::{AnyElement, Hsla, IntoElement, ParentElement, Styled, div, px, rgb};
use nox_core::{IconChoice, ItemPayload, ItemType, NoteColor, PresetIcon};
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

/// The secure-note accent choices from the Pencil "Note Color Field"
/// (`locker.pen` `HFk8V`/`x8fus6`), in the field's left-to-right order.
/// `Neutral` leads the row: it is the "no color" reset, not one of the
/// designed accents.
pub(crate) const NOTE_COLOR_CHOICES: [NoteColor; 6] = [
    NoteColor::Neutral,
    NoteColor::Blue,
    NoteColor::Purple,
    NoteColor::Orange,
    NoteColor::Gold,
    NoteColor::Green,
];

/// The exact Pencil hex token for each [`NoteColor`] variant, except
/// `Neutral`, which has no Pencil token — it borrows the app's own neutral
/// well color so the swatch previews the plain, uncolored icon it resets to.
pub(crate) fn note_color_hsla(color: NoteColor) -> Hsla {
    match color {
        NoteColor::Neutral => Theme::cipher_midnight().raised,
        NoteColor::Blue => rgb(0x4A9FD8).into(),
        NoteColor::Purple => rgb(0x9B7BD7).into(),
        NoteColor::Orange => rgb(0xD98B60).into(),
        NoteColor::Gold => rgb(0xD2B45B).into(),
        NoteColor::Green => rgb(0x58A887).into(),
    }
}

/// Glyph color on a colored note well — white, matching Pencil's colored
/// tiles (e.g. the brand mark's white shield on `#4A9FD8`).
pub(crate) const NOTE_COLOR_GLYPH: Hsla = Hsla {
    h: 0.0,
    s: 0.0,
    l: 1.0,
    a: 1.0,
};

/// A muted well behind a note's icon glyph: same hue as [`note_color_hsla`],
/// darkened and desaturated so the vivid glyph painted on top of it reads
/// clearly instead of blending in. A vivid-on-vivid well (the previous
/// behavior) measures under 2:1 contrast against a grey glyph; this pairing
/// measures 4.7–6.6:1 for every [`NoteColor`], clearing WCAG's 3:1 non-text
/// minimum with room to spare.
pub(crate) fn note_color_wash_hsla(color: NoteColor) -> Hsla {
    let vivid = note_color_hsla(color);
    Hsla {
        h: vivid.h,
        s: vivid.s * 0.5,
        l: 0.16,
        a: 1.0,
    }
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

/// Overwrite `path` with `bytes`, atomically and unconditionally. Unlike
/// `write_private_file`, this never treats an existing file as already
/// correct — used for state that changes across writes (the selections
/// index), never for the content-addressed image cache where identical
/// content really does make an existing file already correct.
fn write_private_file_overwrite(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "cache path has no parent"))?;
    set_private_dir(parent)?;
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
    fs::rename(&temp, path)
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
    write_private_file_overwrite(&selection_file(data_dir), contents.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG relative luminance, then the standard (L1+0.05)/(L2+0.05) ratio.
    fn contrast_ratio(a: Hsla, b: Hsla) -> f32 {
        fn luminance(color: Hsla) -> f32 {
            let rgba = color.to_rgb();
            let channel = |c: f32| {
                if c <= 0.03928 {
                    c / 12.92
                } else {
                    ((c + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(rgba.r) + 0.7152 * channel(rgba.g) + 0.0722 * channel(rgba.b)
        }
        let (l1, l2) = (luminance(a), luminance(b));
        let (hi, lo) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn note_color_wash_holds_contrast_against_its_vivid_glyph() {
        // A note's icon well paints `note_color_wash_hsla` behind a glyph
        // colored with the vivid `note_color_hsla` (see vault_list.rs /
        // detail.rs) — the fix for a glyph that used to render in flat grey,
        // never reflecting the selected color. WCAG's non-text minimum is
        // 3:1; require every choice to clear it with margin.
        //
        // `Neutral` is excluded: it never reaches this pairing in
        // production (render sites fall back to the same neutral well every
        // other item type uses instead), and its `note_color_hsla` is
        // already near-black, so pairing it with its own wash formula would
        // measure a meaningless near-1:1 ratio.
        for color in NOTE_COLOR_CHOICES
            .into_iter()
            .filter(|c| *c != NoteColor::Neutral)
        {
            let ratio = contrast_ratio(note_color_hsla(color), note_color_wash_hsla(color));
            assert!(
                ratio >= 4.5,
                "{color:?} glyph/well contrast is only {ratio:.2}:1"
            );
        }
    }

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

    /// Regression: `persist_local_selections` writes to the same fixed path
    /// every time. Before the fix, `write_private_file`'s exists-check made
    /// every write after the first a silent no-op, so a second selection
    /// never reached disk even though the in-memory map updated correctly.
    #[test]
    fn a_second_selection_overwrites_the_persisted_index() {
        let dir = temp_dir("overwrite");
        let first = cache_local_icon(&dir, "item-a", &valid_png()).unwrap();
        let mut selections = HashMap::new();
        selections.insert(first.item_key.clone(), first.clone());
        persist_local_selections(&dir, &selections).unwrap();
        assert_eq!(load_local_selections(&dir).get("item-a"), Some(&first));

        // A different item entirely: the persisted index must grow, not stay
        // frozen at its first-ever write.
        let second = cache_local_icon(&dir, "item-b", &valid_png()).unwrap();
        selections.insert(second.item_key.clone(), second.clone());
        persist_local_selections(&dir, &selections).unwrap();
        let reloaded = load_local_selections(&dir);
        assert_eq!(reloaded.get("item-a"), Some(&first));
        assert_eq!(reloaded.get("item-b"), Some(&second));

        // Changing the same item's selection must also land on disk.
        let mut other_png = valid_png();
        other_png.push(0);
        let changed = cache_local_icon(&dir, "item-a", &other_png).unwrap();
        selections.insert(changed.item_key.clone(), changed.clone());
        persist_local_selections(&dir, &selections).unwrap();
        let reloaded_again = load_local_selections(&dir);
        assert_eq!(reloaded_again.get("item-a"), Some(&changed));
        assert_ne!(changed.content_hash, first.content_hash);
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
