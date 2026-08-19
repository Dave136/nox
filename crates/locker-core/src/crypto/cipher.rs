//! XChaCha20-Poly1305 payload encryption and canonical change AAD.

use super::secret::{DecryptedPayload, SecretKey};
use crate::{ChangeId, DeviceId, Hlc, ItemId, VaultId};
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Version of the canonical associated-data encoding.
pub const AAD_VERSION: u8 = 1;
/// XChaCha20-Poly1305 nonce size.
pub const NONCE_LENGTH: usize = 24;

/// Journal operation bound into the ciphertext authentication tag.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Operation {
    /// An encrypted item revision.
    Upsert,
    /// An encrypted tombstone with an empty payload.
    Tombstone,
}

/// Alias used by journal code.
pub type ChangeOperation = Operation;

/// Metadata authenticated alongside every encrypted item payload.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AeadContext {
    /// Vault containing the change.
    pub vault_id: VaultId,
    /// Item being revised.
    pub item_id: ItemId,
    /// Immutable change identifier.
    pub change_id: ChangeId,
    /// Revision parents.
    pub parent_change_ids: Vec<ChangeId>,
    /// Device that authored the change.
    pub origin_device_id: DeviceId,
    /// Author's per-vault sequence.
    pub origin_seq: u64,
    /// Author's HLC timestamp.
    pub hlc: Hlc,
    /// Upsert or tombstone.
    pub operation: Operation,
    /// Version of the encrypted item payload schema.
    pub payload_schema_version: u32,
}

/// Alias used when the same metadata is described as associated data.
pub type ChangeAad = AeadContext;
/// Alias used by generic AEAD call sites.
pub type AssociatedData = AeadContext;

impl AeadContext {
    /// Construct the metadata required for canonical AAD.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        vault_id: VaultId,
        item_id: ItemId,
        change_id: ChangeId,
        parent_change_ids: impl AsRef<[ChangeId]>,
        origin_device_id: DeviceId,
        origin_seq: u64,
        hlc: Hlc,
        operation: Operation,
        payload_schema_version: u32,
    ) -> Self {
        let mut parent_change_ids = parent_change_ids.as_ref().to_vec();
        parent_change_ids.sort_unstable();
        Self {
            vault_id,
            item_id,
            change_id,
            parent_change_ids,
            origin_device_id,
            origin_seq,
            hlc,
            operation,
            payload_schema_version,
        }
    }
}

/// A fixed-size XChaCha nonce.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct Nonce([u8; NONCE_LENGTH]);

impl Nonce {
    /// Construct a nonce from canonical bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; NONCE_LENGTH]) -> Self {
        Self(bytes)
    }

    /// Borrow canonical nonce bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; NONCE_LENGTH] {
        &self.0
    }
}

impl AsRef<[u8]> for Nonce {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// Ciphertext and its independently stored nonce.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EncryptedPayload {
    /// Fresh random nonce for this encryption operation.
    pub nonce: Nonce,
    /// Ciphertext including the Poly1305 authentication tag.
    pub ciphertext: Vec<u8>,
}

/// Alias emphasizing that the payload is ciphertext at rest.
pub type Ciphertext = EncryptedPayload;

/// Errors returned by payload encryption/decryption.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CipherError {
    /// OS randomness failed while generating a nonce.
    Randomness,
    /// Canonical AAD serialization failed.
    InvalidAssociatedData,
    /// Authentication failed or ciphertext was malformed.
    InvalidCiphertext,
}

impl fmt::Display for CipherError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Randomness => "randomness unavailable",
            Self::InvalidAssociatedData => "invalid associated data",
            Self::InvalidCiphertext => "invalid ciphertext",
        })
    }
}

impl Operation {
    const fn tag(self) -> u8 {
        match self {
            Self::Upsert => 0,
            Self::Tombstone => 1,
        }
    }
}

impl std::error::Error for CipherError {}

#[derive(Serialize)]
struct CanonicalAad {
    version: u8,
    vault_id: VaultId,
    item_id: ItemId,
    change_id: ChangeId,
    parent_change_ids: Vec<ChangeId>,
    origin_device_id: DeviceId,
    origin_seq: u64,
    hlc: Hlc,
    operation: u8,
    payload_schema_version: u32,
}

/// Encode all change metadata in a stable, versioned binary representation.
pub fn encode_aad(context: &AeadContext) -> Result<Vec<u8>, CipherError> {
    let mut parent_change_ids = context.parent_change_ids.clone();
    parent_change_ids.sort_unstable();
    postcard::to_allocvec(&CanonicalAad {
        version: AAD_VERSION,
        vault_id: context.vault_id,
        item_id: context.item_id,
        change_id: context.change_id,
        parent_change_ids,
        origin_device_id: context.origin_device_id,
        origin_seq: context.origin_seq,
        hlc: context.hlc,
        operation: context.operation.tag(),
        payload_schema_version: context.payload_schema_version,
    })
    .map_err(|_| CipherError::InvalidAssociatedData)
}

/// Alias for [`encode_aad`].
pub use encode_aad as aad_bytes;

/// Encrypt a payload with a fresh random XChaCha20-Poly1305 nonce.
pub fn encrypt(
    key: &SecretKey,
    context: &AeadContext,
    plaintext: impl AsRef<[u8]>,
) -> Result<EncryptedPayload, CipherError> {
    let mut nonce = [0_u8; NONCE_LENGTH];
    getrandom::fill(&mut nonce).map_err(|_| CipherError::Randomness)?;
    let nonce_value = Nonce::from_bytes(nonce);
    let aad = encode_aad(context)?;
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_bytes())
        .map_err(|_| CipherError::InvalidCiphertext)?;
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext.as_ref(),
                aad: &aad,
            },
        )
        .map_err(|_| CipherError::InvalidCiphertext)?;
    Ok(EncryptedPayload {
        nonce: nonce_value,
        ciphertext,
    })
}

/// Decrypt and authenticate an encrypted payload.
pub fn decrypt(
    key: &SecretKey,
    context: &AeadContext,
    encrypted: &EncryptedPayload,
) -> Result<DecryptedPayload, CipherError> {
    let aad = encode_aad(context)?;
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_bytes())
        .map_err(|_| CipherError::InvalidCiphertext)?;
    let plaintext = cipher
        .decrypt(
            XNonce::from_slice(encrypted.nonce.as_bytes()),
            Payload {
                msg: &encrypted.ciphertext,
                aad: &aad,
            },
        )
        .map_err(|_| CipherError::InvalidCiphertext)?;
    Ok(DecryptedPayload::new(plaintext))
}

/// Encrypt an empty tombstone payload while authenticating its metadata.
pub fn encrypt_tombstone(
    key: &SecretKey,
    context: &AeadContext,
) -> Result<EncryptedPayload, CipherError> {
    encrypt(key, context, [])
}

/// Explicitly named aliases for journal call sites.
pub use decrypt as decrypt_payload;
/// Explicitly named aliases for journal call sites.
pub use encrypt as encrypt_payload;
