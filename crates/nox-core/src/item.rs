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
