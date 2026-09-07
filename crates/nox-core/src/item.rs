//! Versioned encrypted item payloads.

use serde::{Deserialize, Serialize};
use std::fmt;

/// The only item payload schema supported by this release.
pub const ITEM_SCHEMA_VERSION: u16 = 1;

/// Item kinds supported by the v1 GUI.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemType {
    /// A username/password login.
    Login,
    /// An arbitrary secure note.
    SecureNote,
}

/// The user's choice of icon for an item — a fixed preset, the site's
/// fetched favicon (Login only), or the item type's built-in default.
///
/// Additive field: never gate this on `ITEM_SCHEMA_VERSION`. An item
/// encrypted before this existed has no `icon` key in its stored JSON;
/// `#[serde(default)]` on the `ItemPayload` field gives it `Default` without
/// touching the version, which `to_json_bytes`/`from_json_bytes` check with
/// hard equality — bumping it would make every existing item unreadable.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IconChoice {
    #[default]
    Default,
    Preset(PresetIcon),
    Favicon,
}

/// The accent color of a secure note — the fixed Pencil "Note Color Field"
/// choices. `gui` owns each variant's hex token; this crate only names the
/// choice.
///
/// `Neutral` is the "no color" state: a new note starts here and renders
/// with the same neutral well/glyph as every other item type, until the
/// user explicitly picks one of the accent colors below.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteColor {
    #[default]
    Neutral,
    Blue,
    Purple,
    Orange,
    Gold,
    Green,
}

/// A curated, fixed set of bundled icons the user can pick per item.
/// `gui` owns the SVG path and label for each variant — this crate only
/// names the choice.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PresetIcon {
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
}

/// Plaintext item data stored inside an encrypted journal revision.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ItemPayload {
    /// Version of this JSON payload schema.
    pub schema_version: u16,
    /// Item kind, serialized as `login` or `secure_note`.
    pub item_type: ItemType,
    /// Human-readable item title.
    pub title: String,
    /// Login username, or empty for secure notes.
    pub username: String,
    /// Login password, or empty for secure notes.
    pub password: String,
    /// Associated website or resource URIs.
    pub uris: Vec<String>,
    /// Free-form secure notes.
    pub notes: String,
    /// Application creation timestamp in milliseconds since Unix epoch.
    pub created_at: u64,
    /// Application update timestamp in milliseconds since Unix epoch.
    pub updated_at: u64,
    /// The user's chosen icon for this item. Additive — see [`IconChoice`].
    #[serde(default)]
    pub icon: IconChoice,
    /// The secure note's accent color. Additive — see [`IconChoice`].
    #[serde(default)]
    pub note_color: NoteColor,
}

/// Errors returned while encoding or decoding an item payload.
#[derive(Debug)]
pub enum ItemPayloadError {
    /// The JSON document could not be encoded or decoded.
    Json(serde_json::Error),
    /// The document uses a schema version this build does not understand.
    UnsupportedSchemaVersion(u16),
}

impl fmt::Display for ItemPayloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(formatter, "item payload JSON error: {error}"),
            Self::UnsupportedSchemaVersion(version) => {
                write!(
                    formatter,
                    "unsupported item payload schema version {version}"
                )
            }
        }
    }
}

impl std::error::Error for ItemPayloadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::UnsupportedSchemaVersion(_) => None,
        }
    }
}

impl From<serde_json::Error> for ItemPayloadError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl ItemPayload {
    /// Encode this payload as versioned JSON bytes.
    pub fn to_json_bytes(&self) -> Result<Vec<u8>, ItemPayloadError> {
        if self.schema_version != ITEM_SCHEMA_VERSION {
            return Err(ItemPayloadError::UnsupportedSchemaVersion(
                self.schema_version,
            ));
        }
        serde_json::to_vec(self).map_err(Into::into)
    }

    /// Decode JSON bytes, rejecting schema versions that are not supported.
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, ItemPayloadError> {
        let payload: Self = serde_json::from_slice(bytes)?;
        if payload.schema_version != ITEM_SCHEMA_VERSION {
            return Err(ItemPayloadError::UnsupportedSchemaVersion(
                payload.schema_version,
            ));
        }
        Ok(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(item_type: ItemType) -> ItemPayload {
        ItemPayload {
            schema_version: ITEM_SCHEMA_VERSION,
            item_type,
            title: "Example".into(),
            username: "alice".into(),
            password: "secret".into(),
            uris: vec!["https://example.test".into()],
            notes: "notes".into(),
            created_at: 1_700_000_000_000,
            updated_at: 1_700_000_000_001,
            icon: IconChoice::Default,
            note_color: NoteColor::Blue,
        }
    }

    #[test]
    fn login_and_secure_note_round_trip_through_json() {
        for item_type in [ItemType::Login, ItemType::SecureNote] {
            let original = payload(item_type);
            let encoded = original.to_json_bytes().unwrap();
            let decoded = ItemPayload::from_json_bytes(&encoded).unwrap();
            assert_eq!(decoded, original);
        }
    }

    #[test]
    fn icon_choice_round_trips_through_json_for_every_variant() {
        let choices = [
            IconChoice::Default,
            IconChoice::Preset(PresetIcon::Globe),
            IconChoice::Preset(PresetIcon::GraduationCap),
            IconChoice::Favicon,
        ];
        for icon in choices {
            let mut original = payload(ItemType::Login);
            original.icon = icon;
            let encoded = original.to_json_bytes().unwrap();
            let decoded = ItemPayload::from_json_bytes(&encoded).unwrap();
            assert_eq!(decoded, original);
        }
    }

    #[test]
    fn item_json_without_an_icon_key_defaults_to_icon_default() {
        // Stands in for an item encrypted before this field existed: its
        // stored JSON has no `icon` key at all, and `schema_version` is
        // unchanged — ITEM_SCHEMA_VERSION must not bump for this field.
        let mut value = serde_json::to_value(payload(ItemType::Login)).unwrap();
        value.as_object_mut().unwrap().remove("icon");
        let encoded = serde_json::to_vec(&value).unwrap();

        let decoded = ItemPayload::from_json_bytes(&encoded).unwrap();
        assert_eq!(decoded.icon, IconChoice::Default);
        assert_eq!(decoded.schema_version, ITEM_SCHEMA_VERSION);
    }

    #[test]
    fn note_color_round_trips_through_json_for_every_variant() {
        for note_color in [
            NoteColor::Neutral,
            NoteColor::Blue,
            NoteColor::Purple,
            NoteColor::Orange,
            NoteColor::Gold,
            NoteColor::Green,
        ] {
            let mut original = payload(ItemType::SecureNote);
            original.note_color = note_color;
            let encoded = original.to_json_bytes().unwrap();
            let decoded = ItemPayload::from_json_bytes(&encoded).unwrap();
            assert_eq!(decoded, original);
        }
    }

    #[test]
    fn item_json_without_a_note_color_key_defaults_to_neutral() {
        // Stands in for a note encrypted before this field existed: its
        // stored JSON has no `note_color` key at all, and `schema_version`
        // is unchanged — ITEM_SCHEMA_VERSION must not bump for this field.
        // Neutral, not a color, so an old note keeps its plain icon instead
        // of surfacing an accent nobody chose.
        let mut value = serde_json::to_value(payload(ItemType::SecureNote)).unwrap();
        value.as_object_mut().unwrap().remove("note_color");
        let encoded = serde_json::to_vec(&value).unwrap();

        let decoded = ItemPayload::from_json_bytes(&encoded).unwrap();
        assert_eq!(decoded.note_color, NoteColor::Neutral);
        assert_eq!(decoded.schema_version, ITEM_SCHEMA_VERSION);
    }

    #[test]
    fn unknown_schema_version_is_rejected() {
        let mut value = serde_json::to_value(payload(ItemType::Login)).unwrap();
        value["schema_version"] = serde_json::json!(2);
        let encoded = serde_json::to_vec(&value).unwrap();

        assert!(matches!(
            ItemPayload::from_json_bytes(&encoded),
            Err(ItemPayloadError::UnsupportedSchemaVersion(2))
        ));
    }
}
