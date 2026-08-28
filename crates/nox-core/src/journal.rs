//! Immutable, signed, encrypted change journal operations.

use crate::{
    Hlc, HlcClock,
    crypto::{
        CipherError, Ed25519Keypair, EncryptedPayload, KeyError, Operation, SecretKey,
        cipher::{self, AeadContext, Nonce},
        keys,
    },
    ids::{ChangeId, DeviceId, ItemId, VaultId},
    membership::{self, AuthorizationSnapshot, MembershipRecord, MembershipRecordHash},
    merge,
    storage::Db,
};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params, types::Type};
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    num::TryFromIntError,
};

/// Maximum encrypted payload stored in one journal change.
pub const MAX_CHANGE_CIPHERTEXT_BYTES: usize = 8 * 1024 * 1024;
/// Maximum canonical wire representation of one journal change.
pub const MAX_ENCODED_CHANGE_BYTES: usize = MAX_CHANGE_CIPHERTEXT_BYTES + 64 * 1024;
/// Maximum number of changes in one atomic replication batch.
pub const MAX_CHANGES_PER_BATCH: usize = 64;
/// Maximum encoded bytes in one normal replication batch.
pub const MAX_BATCH_PLAINTEXT_BYTES: usize = 60 * 1024;
/// Maximum number of cursor entries accepted from a validated membership set.
pub const MAX_CURSOR_ENTRIES: usize = membership::MAX_ACTIVE_MEMBERS;

pub type CursorMap = BTreeMap<DeviceId, u64>;

const CHANGE_WIRE_MAGIC: &[u8; 4] = b"LCH1";

/// Result of applying an authenticated remote change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplyResult {
    /// The change was inserted into the journal.
    Inserted,
    /// The exact same signed change was already present.
    Duplicate,
}

/// Errors returned by journal creation and application.
#[derive(Debug)]
pub enum JournalError {
    /// SQLite rejected a journal operation.
    Sql(rusqlite::Error),
    /// A payload could not be encrypted or its metadata encoded.
    Cipher(CipherError),
    /// A signature or key encoding was invalid.
    Key(KeyError),
    /// A persisted counter or identifier did not fit its SQLite representation.
    NumericOverflow,
    /// A HLC could not advance.
    Clock(crate::HlcError),
    /// The signed change failed structural validation.
    InvalidChange(&'static str),
    /// The supplied signature did not authenticate the change.
    InvalidSignature,
    /// An existing change id or origin sequence names different signed bytes.
    ConflictingDuplicate,
    /// Canonical journal encoding failed.
    Encoding,
    /// A persisted change exceeds the supported ciphertext ceiling.
    UnsupportedStoredChange,
}

impl fmt::Display for JournalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sql(error) => write!(formatter, "journal SQLite error: {error}"),
            Self::Cipher(error) => write!(formatter, "journal cipher error: {error}"),
            Self::Key(error) => write!(formatter, "journal key error: {error}"),
            Self::NumericOverflow => formatter.write_str("journal counter exceeds SQLite range"),
            Self::Clock(error) => write!(formatter, "journal clock error: {error}"),
            Self::InvalidChange(reason) => write!(formatter, "invalid journal change: {reason}"),
            Self::InvalidSignature => formatter.write_str("invalid journal signature"),
            Self::ConflictingDuplicate => {
                formatter.write_str("conflicting duplicate journal change")
            }
            Self::Encoding => formatter.write_str("journal encoding failed"),
            Self::UnsupportedStoredChange => {
                formatter.write_str("stored journal change exceeds the supported size")
            }
        }
    }
}

impl std::error::Error for JournalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sql(error) => Some(error),
            Self::Cipher(error) => Some(error),
            Self::Key(error) => Some(error),
            Self::Clock(error) => Some(error),
            Self::NumericOverflow
            | Self::InvalidChange(_)
            | Self::InvalidSignature
            | Self::ConflictingDuplicate
            | Self::Encoding
            | Self::UnsupportedStoredChange => None,
        }
    }
}

impl From<rusqlite::Error> for JournalError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sql(error)
    }
}

impl From<CipherError> for JournalError {
    fn from(error: CipherError) -> Self {
        Self::Cipher(error)
    }
}

impl From<KeyError> for JournalError {
    fn from(error: KeyError) -> Self {
        Self::Key(error)
    }
}

impl From<crate::HlcError> for JournalError {
    fn from(error: crate::HlcError) -> Self {
        Self::Clock(error)
    }
}

impl From<TryFromIntError> for JournalError {
    fn from(_: TryFromIntError) -> Self {
        Self::NumericOverflow
    }
}

/// A complete immutable journal revision.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Change {
    /// Randomly generated immutable change identifier.
    pub change_id: ChangeId,
    /// Vault containing the revision.
    pub vault_id: VaultId,
    /// Item being revised.
    pub item_id: ItemId,
    /// Revision ancestry, canonically ordered by id bytes.
    pub parent_change_ids: Vec<ChangeId>,
    /// Device that authored the change.
    pub origin_device_id: DeviceId,
    /// Per-vault sequence allocated by the author.
    pub origin_seq: u64,
    /// Persistent HLC timestamp assigned by the author.
    pub hlc: Hlc,
    /// Upsert or tombstone operation.
    pub operation: Operation,
    /// Version of the encrypted payload schema.
    pub payload_schema_version: u32,
    /// Fresh XChaCha20-Poly1305 nonce.
    pub nonce: Nonce,
    /// Encrypted resulting item payload, including its authentication tag.
    pub ciphertext: Vec<u8>,
    /// Ed25519 signature over all preceding fields in canonical form.
    pub signature: Vec<u8>,
}

#[derive(Serialize)]
struct UnsignedChange<'a> {
    change_id: ChangeId,
    vault_id: VaultId,
    item_id: ItemId,
    parent_change_ids: &'a [ChangeId],
    origin_device_id: DeviceId,
    origin_seq: u64,
    hlc: Hlc,
    operation: u8,
    payload_schema_version: u32,
    nonce: Nonce,
    ciphertext: &'a [u8],
}

impl Change {
    /// Build and sign a change from encrypted payload material.
    #[allow(clippy::too_many_arguments)]
    pub fn new_signed(
        vault_id: VaultId,
        item_id: ItemId,
        parent_change_ids: impl AsRef<[ChangeId]>,
        origin_device_id: DeviceId,
        origin_seq: u64,
        hlc: Hlc,
        operation: Operation,
        payload_schema_version: u32,
        encrypted: &EncryptedPayload,
        signing_key: &Ed25519Keypair,
    ) -> Result<Self, JournalError> {
        Self::new_signed_with_id(
            ChangeId::new(),
            vault_id,
            item_id,
            parent_change_ids,
            origin_device_id,
            origin_seq,
            hlc,
            operation,
            payload_schema_version,
            encrypted,
            signing_key,
        )
    }

    /// Build and sign a change with a caller-supplied immutable id.
    #[allow(clippy::too_many_arguments)]
    pub fn new_signed_with_id(
        change_id: ChangeId,
        vault_id: VaultId,
        item_id: ItemId,
        parent_change_ids: impl AsRef<[ChangeId]>,
        origin_device_id: DeviceId,
        origin_seq: u64,
        hlc: Hlc,
        operation: Operation,
        payload_schema_version: u32,
        encrypted: &EncryptedPayload,
        signing_key: &Ed25519Keypair,
    ) -> Result<Self, JournalError> {
        let mut change = Self {
            change_id,
            vault_id,
            item_id,
            parent_change_ids: parent_change_ids.as_ref().to_vec(),
            origin_device_id,
            origin_seq,
            hlc,
            operation,
            payload_schema_version,
            nonce: encrypted.nonce,
            ciphertext: encrypted.ciphertext.clone(),
            signature: Vec::new(),
        };
        change.parent_change_ids.sort_unstable();
        change.signature = signing_key.sign(&change.signed_bytes()?).to_vec();
        Ok(change)
    }

    /// Return the canonical bytes authenticated by the Ed25519 signature.
    pub fn signed_bytes(&self) -> Result<Vec<u8>, JournalError> {
        let mut parent_change_ids = self.parent_change_ids.clone();
        parent_change_ids.sort_unstable();
        postcard::to_allocvec(&UnsignedChange {
            change_id: self.change_id,
            vault_id: self.vault_id,
            item_id: self.item_id,
            parent_change_ids: &parent_change_ids,
            origin_device_id: self.origin_device_id,
            origin_seq: self.origin_seq,
            hlc: self.hlc,
            operation: operation_tag(self.operation),
            payload_schema_version: self.payload_schema_version,
            nonce: self.nonce,
            ciphertext: &self.ciphertext,
        })
        .map_err(|_| JournalError::Encoding)
    }

    /// Reconstruct the AEAD metadata for this change.
    #[must_use]
    pub fn aad_context(&self) -> AeadContext {
        AeadContext::new(
            self.vault_id,
            self.item_id,
            self.change_id,
            &self.parent_change_ids,
            self.origin_device_id,
            self.origin_seq,
            self.hlc,
            self.operation,
            self.payload_schema_version,
        )
    }

    /// Reconstruct the stored encrypted payload.
    #[must_use]
    pub fn encrypted_payload(&self) -> EncryptedPayload {
        EncryptedPayload {
            nonce: self.nonce,
            ciphertext: self.ciphertext.clone(),
        }
    }

    /// Whether this change is an encrypted tombstone.
    #[must_use]
    pub const fn is_tombstone(&self) -> bool {
        matches!(self.operation, Operation::Tombstone)
    }

    fn canonicalized(&self) -> Self {
        let mut change = self.clone();
        change.parent_change_ids.sort_unstable();
        change
    }

    pub(crate) fn validate(&self) -> Result<(), JournalError> {
        if self.origin_seq == 0 {
            return Err(JournalError::InvalidChange("origin sequence starts at one"));
        }
        if self.parent_change_ids.len() > MAX_CURSOR_ENTRIES {
            return Err(JournalError::InvalidChange("too many parent changes"));
        }
        if self
            .parent_change_ids
            .windows(2)
            .any(|parents| parents[0] == parents[1])
        {
            return Err(JournalError::InvalidChange("duplicate parent change id"));
        }
        if self.ciphertext.len() < 16 {
            return Err(JournalError::InvalidChange(
                "ciphertext is shorter than an AEAD tag",
            ));
        }
        if self.ciphertext.len() > MAX_CHANGE_CIPHERTEXT_BYTES {
            return Err(JournalError::UnsupportedStoredChange);
        }
        if self.signature.len() != 64 {
            return Err(JournalError::InvalidChange("signature length"));
        }
        Ok(())
    }
}

/// Create, encrypt, sign, and atomically persist a local change.
#[allow(clippy::too_many_arguments)]
pub fn create_local_change(
    db: &mut Db,
    vault_id: VaultId,
    item_id: ItemId,
    signing_key: &Ed25519Keypair,
    encryption_key: &SecretKey,
    operation: Operation,
    payload: impl AsRef<[u8]>,
    payload_schema_version: u32,
    wall_clock_ms: u64,
) -> Result<Change, JournalError> {
    db.transaction(|tx| {
        let parent_change_ids = current_parent_ids(tx, item_id)?;
        create_local_change_in_transaction(
            tx,
            vault_id,
            item_id,
            parent_change_ids,
            signing_key,
            encryption_key,
            operation,
            payload.as_ref(),
            payload_schema_version,
            wall_clock_ms,
        )
    })
}

/// Create a local revision with explicit parents, used for manual resolution.
#[allow(clippy::too_many_arguments)]
pub(crate) fn create_local_change_with_parents(
    db: &mut Db,
    vault_id: VaultId,
    item_id: ItemId,
    parent_change_ids: Vec<ChangeId>,
    signing_key: &Ed25519Keypair,
    encryption_key: &SecretKey,
    operation: Operation,
    payload: impl AsRef<[u8]>,
    payload_schema_version: u32,
    wall_clock_ms: u64,
) -> Result<Change, JournalError> {
    db.transaction(|tx| {
        create_local_change_in_transaction(
            tx,
            vault_id,
            item_id,
            parent_change_ids,
            signing_key,
            encryption_key,
            operation,
            payload.as_ref(),
            payload_schema_version,
            wall_clock_ms,
        )
    })
}

#[allow(clippy::too_many_arguments)]
fn create_local_change_in_transaction(
    tx: &Transaction<'_>,
    vault_id: VaultId,
    item_id: ItemId,
    mut parent_change_ids: Vec<ChangeId>,
    signing_key: &Ed25519Keypair,
    encryption_key: &SecretKey,
    operation: Operation,
    payload: &[u8],
    payload_schema_version: u32,
    wall_clock_ms: u64,
) -> Result<Change, JournalError> {
    if payload.len() > MAX_CHANGE_CIPHERTEXT_BYTES.saturating_sub(16) {
        return Err(JournalError::UnsupportedStoredChange);
    }
    let origin_device_id = DeviceId::from_public_key(signing_key.public_key_bytes());
    parent_change_ids.sort_unstable();
    let (current_hlc, current_origin_seq) = local_clock_state(tx, vault_id, origin_device_id)?;
    let mut clock = HlcClock::from_state(current_hlc, current_origin_seq);
    let stamp = clock.next(wall_clock_ms)?;
    let change_id = ChangeId::new();
    let context = AeadContext::new(
        vault_id,
        item_id,
        change_id,
        &parent_change_ids,
        origin_device_id,
        stamp.origin_seq,
        stamp.hlc,
        operation,
        payload_schema_version,
    );
    let encrypted = if matches!(operation, Operation::Tombstone) {
        cipher::encrypt_tombstone(encryption_key, &context)?
    } else {
        cipher::encrypt(encryption_key, &context, payload)?
    };
    let change = Change::new_signed_with_id(
        change_id,
        vault_id,
        item_id,
        parent_change_ids,
        origin_device_id,
        stamp.origin_seq,
        stamp.hlc,
        operation,
        payload_schema_version,
        &encrypted,
        signing_key,
    )?;
    insert_new_change(tx, &change)?;
    advance_sync_cursor(tx, origin_device_id)?;
    persist_clock_state(tx, vault_id, stamp.hlc, stamp.origin_seq)?;
    merge::rebuild_item_projection(tx, item_id)?;
    Ok(change)
}

/// Authenticate and atomically insert a remote change.
///
/// This updates the journal, replication cursor, clock state, and deterministic
/// item/conflict projection in one transaction.
pub fn apply_received_change(
    db: &mut Db,
    change: &Change,
    public_key: impl AsRef<[u8]>,
) -> Result<ApplyResult, JournalError> {
    let change = change.canonicalized();
    change.validate()?;
    let public_key = public_key.as_ref();
    if DeviceId::try_from_public_key(public_key).map_err(|_| JournalError::InvalidSignature)?
        != change.origin_device_id
    {
        return Err(JournalError::InvalidSignature);
    }
    keys::verify_signature(public_key, &change.signed_bytes()?, &change.signature)
        .map_err(|_| JournalError::InvalidSignature)?;

    db.transaction(|tx| {
        let (inserted, _) = apply_changes_in_transaction(tx, std::slice::from_ref(&change))?;
        Ok(if inserted == 0 {
            ApplyResult::Duplicate
        } else {
            ApplyResult::Inserted
        })
    })
}

fn operation_tag(operation: Operation) -> u8 {
    match operation {
        Operation::Upsert => 0,
        Operation::Tombstone => 1,
    }
}

fn operation_from_tag(tag: i64) -> rusqlite::Result<Operation> {
    match tag {
        0 => Ok(Operation::Upsert),
        1 => Ok(Operation::Tombstone),
        _ => Err(rusqlite::Error::FromSqlConversionFailure(
            7,
            Type::Integer,
            "invalid journal operation".into(),
        )),
    }
}

fn current_parent_ids(
    tx: &Transaction<'_>,
    item_id: ItemId,
) -> Result<Vec<ChangeId>, JournalError> {
    let winning_change_id = tx
        .query_row(
            "SELECT winning_change_id FROM items WHERE item_id = ?1",
            [item_id.as_ref()],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?;
    let Some(winning_change_id) = winning_change_id else {
        return Ok(Vec::new());
    };
    Ok(vec![ChangeId::from_bytes(fixed_bytes(
        winning_change_id,
        0,
    )?)])
}

fn local_clock_state(
    tx: &Transaction<'_>,
    vault_id: VaultId,
    origin_device_id: DeviceId,
) -> Result<(Hlc, u64), JournalError> {
    let state = tx
        .query_row(
            "SELECT hlc_physical_ms, hlc_logical, next_origin_seq FROM clock_state WHERE vault_id = ?1",
            [vault_id.as_ref()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((physical_ms, logical, origin_seq)) = state else {
        let max_seq = tx.query_row(
            "SELECT COALESCE(MAX(origin_seq), 0) FROM changes WHERE vault_id = ?1 AND origin_device_id = ?2",
            params![vault_id.as_ref(), origin_device_id.as_ref()],
            |row| row.get::<_, i64>(0),
        )?;
        return Ok((Hlc::new(0, 0), u64::try_from(max_seq)?));
    };
    let max_seq = tx.query_row(
        "SELECT COALESCE(MAX(origin_seq), 0) FROM changes WHERE vault_id = ?1 AND origin_device_id = ?2",
        params![vault_id.as_ref(), origin_device_id.as_ref()],
        |row| row.get::<_, i64>(0),
    )?;
    Ok((
        Hlc::new(u64::try_from(physical_ms)?, u32::try_from(logical)?),
        u64::try_from(origin_seq)?.max(u64::try_from(max_seq)?),
    ))
}

fn persist_clock_state(
    tx: &Transaction<'_>,
    vault_id: VaultId,
    hlc: Hlc,
    origin_seq: u64,
) -> Result<(), JournalError> {
    tx.execute(
        "INSERT INTO clock_state (vault_id, hlc_physical_ms, hlc_logical, next_origin_seq)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(vault_id) DO UPDATE SET
            hlc_physical_ms = excluded.hlc_physical_ms,
            hlc_logical = excluded.hlc_logical,
            next_origin_seq = excluded.next_origin_seq",
        params![
            vault_id.as_ref(),
            i64::try_from(hlc.physical_ms)?,
            i64::from(hlc.logical),
            i64::try_from(origin_seq)?,
        ],
    )?;
    Ok(())
}

pub(crate) fn insert_new_change(tx: &Transaction<'_>, change: &Change) -> Result<(), JournalError> {
    change.validate()?;
    tx.execute(
        "INSERT INTO changes (
            change_id, vault_id, item_id, parent_change_ids, origin_device_id,
            origin_seq, hlc_physical_ms, hlc_logical, operation,
            payload_schema_version, nonce, ciphertext, signature
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            change.change_id.as_ref(),
            change.vault_id.as_ref(),
            change.item_id.as_ref(),
            postcard::to_allocvec(&change.parent_change_ids).map_err(|_| JournalError::Encoding)?,
            change.origin_device_id.as_ref(),
            i64::try_from(change.origin_seq)?,
            i64::try_from(change.hlc.physical_ms)?,
            i64::from(change.hlc.logical),
            i64::from(operation_tag(change.operation)),
            i64::from(change.payload_schema_version),
            change.nonce.as_ref(),
            &change.ciphertext,
            change.signature.as_slice(),
        ],
    )?;
    Ok(())
}

fn check_duplicate(
    tx: &Transaction<'_>,
    change: &Change,
) -> Result<Option<ApplyResult>, JournalError> {
    let existing_by_id = load_change_by_id(tx, change.change_id)?;
    if let Some(existing) = existing_by_id {
        return if same_signed_change(&existing, change)? {
            Ok(Some(ApplyResult::Duplicate))
        } else {
            Err(JournalError::ConflictingDuplicate)
        };
    }

    let existing_by_origin = tx
        .query_row(
            "SELECT change_id, vault_id, item_id, parent_change_ids, origin_device_id,
                    origin_seq, hlc_physical_ms, hlc_logical, operation,
                    payload_schema_version, nonce, ciphertext, signature
             FROM changes WHERE origin_device_id = ?1 AND origin_seq = ?2",
            params![
                change.origin_device_id.as_ref(),
                i64::try_from(change.origin_seq)?
            ],
            row_to_change,
        )
        .optional()?;
    if let Some(existing) = existing_by_origin {
        return if same_signed_change(&existing, change)? {
            Ok(Some(ApplyResult::Duplicate))
        } else {
            Err(JournalError::ConflictingDuplicate)
        };
    }
    Ok(None)
}

fn same_signed_change(left: &Change, right: &Change) -> Result<bool, JournalError> {
    Ok(left.signature == right.signature && left.signed_bytes()? == right.signed_bytes()?)
}

fn load_change_by_id(
    tx: &Transaction<'_>,
    change_id: ChangeId,
) -> Result<Option<Change>, JournalError> {
    tx.query_row(
        "SELECT change_id, vault_id, item_id, parent_change_ids, origin_device_id,
                origin_seq, hlc_physical_ms, hlc_logical, operation,
                payload_schema_version, nonce, ciphertext, signature
         FROM changes WHERE change_id = ?1",
        [change_id.as_ref()],
        row_to_change,
    )
    .optional()
    .map_err(JournalError::from)
}

pub(crate) fn row_to_change(row: &Row<'_>) -> rusqlite::Result<Change> {
    let parent_bytes = row.get::<_, Vec<u8>>(3)?;
    let parent_change_ids = postcard::from_bytes(&parent_bytes).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(3, Type::Blob, "invalid parent encoding".into())
    })?;
    Ok(Change {
        change_id: ChangeId::from_bytes(fixed_bytes(row.get(0)?, 0)?),
        vault_id: VaultId::from_bytes(fixed_bytes(row.get(1)?, 1)?),
        item_id: ItemId::from_bytes(fixed_bytes(row.get(2)?, 2)?),
        parent_change_ids,
        origin_device_id: DeviceId::from_bytes(fixed_bytes(row.get(4)?, 4)?),
        origin_seq: u64::try_from(row.get::<_, i64>(5)?).map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                5,
                Type::Integer,
                "negative origin sequence".into(),
            )
        })?,
        hlc: Hlc::new(
            u64::try_from(row.get::<_, i64>(6)?).map_err(|_| {
                rusqlite::Error::FromSqlConversionFailure(
                    6,
                    Type::Integer,
                    "negative HLC physical time".into(),
                )
            })?,
            u32::try_from(row.get::<_, i64>(7)?).map_err(|_| {
                rusqlite::Error::FromSqlConversionFailure(
                    7,
                    Type::Integer,
                    "invalid HLC logical counter".into(),
                )
            })?,
        ),
        operation: operation_from_tag(row.get(8)?)?,
        payload_schema_version: u32::try_from(row.get::<_, i64>(9)?).map_err(|_| {
            rusqlite::Error::FromSqlConversionFailure(
                9,
                Type::Integer,
                "invalid payload schema version".into(),
            )
        })?,
        nonce: Nonce::from_bytes(fixed_bytes(row.get(10)?, 10)?),
        ciphertext: row.get(11)?,
        signature: row.get(12)?,
    })
}

pub(crate) fn load_item_changes(
    connection: &Connection,
    item_id: ItemId,
) -> Result<Vec<Change>, JournalError> {
    let mut statement = connection.prepare(
        "SELECT change_id, vault_id, item_id, parent_change_ids, origin_device_id,
                origin_seq, hlc_physical_ms, hlc_logical, operation,
                payload_schema_version, nonce, ciphertext, signature
         FROM changes WHERE item_id = ?1",
    )?;
    Ok(statement
        .query_map([item_id.as_ref()], row_to_change)?
        .collect::<Result<Vec<_>, _>>()?)
}

pub(crate) fn load_all_changes(
    connection: &Connection,
    vault_id: VaultId,
) -> Result<Vec<Change>, JournalError> {
    let mut statement = connection.prepare(
        "SELECT change_id, vault_id, item_id, parent_change_ids, origin_device_id,
                origin_seq, hlc_physical_ms, hlc_logical, operation,
                payload_schema_version, nonce, ciphertext, signature
         FROM changes WHERE vault_id = ?1",
    )?;
    Ok(statement
        .query_map([vault_id.as_ref()], row_to_change)?
        .collect::<Result<Vec<_>, _>>()?)
}

fn fixed_bytes<const N: usize>(bytes: Vec<u8>, index: usize) -> rusqlite::Result<[u8; N]> {
    bytes.try_into().map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            Type::Blob,
            "invalid fixed-size bytes".into(),
        )
    })
}

fn advance_sync_cursor(
    tx: &Transaction<'_>,
    origin_device_id: DeviceId,
) -> Result<(), JournalError> {
    let current = tx
        .query_row(
            "SELECT highest_contiguous_origin_seq FROM sync_cursors WHERE origin_device_id = ?1",
            [origin_device_id.as_ref()],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .map(u64::try_from)
        .transpose()?;
    let current = current.unwrap_or(0);
    let mut contiguous = current;
    loop {
        let next = contiguous
            .checked_add(1)
            .ok_or(JournalError::NumericOverflow)?;
        let present = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM changes WHERE origin_device_id = ?1 AND origin_seq = ?2)",
            params![origin_device_id.as_ref(), i64::try_from(next)?],
            |row| row.get::<_, bool>(0),
        )?;
        if !present {
            break;
        }
        contiguous = next;
    }
    tx.execute(
        "INSERT INTO sync_cursors (origin_device_id, highest_contiguous_origin_seq) VALUES (?1, ?2)
         ON CONFLICT(origin_device_id) DO UPDATE SET highest_contiguous_origin_seq = excluded.highest_contiguous_origin_seq",
        params![origin_device_id.as_ref(), i64::try_from(contiguous)?],
    )?;
    Ok(())
}

fn merge_remote_clock(
    tx: &Transaction<'_>,
    vault_id: VaultId,
    remote_hlc: Hlc,
) -> Result<(), JournalError> {
    let current = tx
        .query_row(
            "SELECT hlc_physical_ms, hlc_logical, next_origin_seq FROM clock_state WHERE vault_id = ?1",
            [vault_id.as_ref()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((physical_ms, logical, next_origin_seq)) = current else {
        return persist_clock_state(tx, vault_id, remote_hlc, 0);
    };
    let current_hlc = Hlc::new(u64::try_from(physical_ms)?, u32::try_from(logical)?);
    if remote_hlc > current_hlc {
        persist_clock_state(tx, vault_id, remote_hlc, u64::try_from(next_origin_seq)?)?;
    }
    Ok(())
}

/// Errors exposed by the bounded replication storage facade.
#[derive(Debug)]
pub enum ReplicationStoreError {
    Sql(rusqlite::Error),
    Journal(JournalError),
    Membership(membership::MembershipError),
    WrongVault,
    InvalidCursor,
    UnknownOrigin,
    BlockedAuthor,
    BatchTooLarge,
    ChangeTooLarge,
    InvalidEncoding,
    UnsupportedStoredChange,
}

impl fmt::Display for ReplicationStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sql(error) => write!(formatter, "replication SQLite error: {error}"),
            Self::Journal(error) => write!(formatter, "replication journal error: {error}"),
            Self::Membership(error) => write!(formatter, "replication membership error: {error}"),
            Self::WrongVault => formatter.write_str("replication vault mismatch"),
            Self::InvalidCursor => formatter.write_str("invalid replication cursor"),
            Self::UnknownOrigin => formatter.write_str("unknown replication origin"),
            Self::BlockedAuthor => formatter.write_str("blocked replication author"),
            Self::BatchTooLarge => formatter.write_str("replication batch is too large"),
            Self::ChangeTooLarge => formatter.write_str("replication change is too large"),
            Self::InvalidEncoding => formatter.write_str("invalid replication change encoding"),
            Self::UnsupportedStoredChange => {
                formatter.write_str("stored replication change exceeds the supported size")
            }
        }
    }
}

impl std::error::Error for ReplicationStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sql(error) => Some(error),
            Self::Journal(error) => Some(error),
            Self::Membership(error) => Some(error),
            Self::WrongVault
            | Self::InvalidCursor
            | Self::UnknownOrigin
            | Self::BlockedAuthor
            | Self::BatchTooLarge
            | Self::ChangeTooLarge
            | Self::InvalidEncoding
            | Self::UnsupportedStoredChange => None,
        }
    }
}

impl From<rusqlite::Error> for ReplicationStoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sql(error)
    }
}

impl From<JournalError> for ReplicationStoreError {
    fn from(error: JournalError) -> Self {
        if matches!(error, JournalError::UnsupportedStoredChange) {
            Self::UnsupportedStoredChange
        } else {
            Self::Journal(error)
        }
    }
}

impl From<membership::MembershipError> for ReplicationStoreError {
    fn from(error: membership::MembershipError) -> Self {
        Self::Membership(error)
    }
}

/// Result of applying one authenticated replication batch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BatchApplyResult {
    pub inserted: usize,
    pub duplicates: usize,
    pub cursors_after_commit: BTreeMap<DeviceId, u64>,
}

/// An independent, key-free SQLite facade used by one replication session.
pub struct ReplicationStore {
    db: Db,
    vault_id: VaultId,
}

impl fmt::Debug for ReplicationStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ReplicationStore(<redacted>)")
    }
}

impl ReplicationStore {
    pub(crate) fn new(db: Db, vault_id: VaultId) -> Self {
        Self { db, vault_id }
    }

    #[must_use]
    pub fn vault_id(&self) -> VaultId {
        self.vault_id
    }

    pub fn authorization_snapshot(&self) -> Result<AuthorizationSnapshot, ReplicationStoreError> {
        Ok(membership::load_authorization(&self.db, self.vault_id)?)
    }

    pub fn membership_records(&self) -> Result<Vec<MembershipRecord>, ReplicationStoreError> {
        Ok(self
            .authorization_snapshot()?
            .membership
            .records()
            .cloned()
            .collect())
    }

    pub fn membership_set_hash(&self) -> Result<[u8; 32], ReplicationStoreError> {
        let records = self.membership_records()?;
        membership_set_hash(&records)
    }

    /// Insert a validated membership subset without activating an incomplete chain.
    pub fn merge_membership_records(
        &mut self,
        incoming: &[MembershipRecord],
    ) -> Result<(), ReplicationStoreError> {
        let mut records = self.membership_records()?;
        records.extend(incoming.iter().cloned());
        let validated = membership::validate_membership(self.vault_id, &records)?;
        let unique = validated.records().cloned().collect::<Vec<_>>();
        self.db.transaction(|tx| {
            for record in &unique {
                membership::insert_membership_record_tx(tx, record)?;
            }
            Ok::<_, ReplicationStoreError>(())
        })?;
        Ok(())
    }

    pub fn cursor_map(&self) -> Result<BTreeMap<DeviceId, u64>, ReplicationStoreError> {
        let authorization = self.authorization_snapshot()?;
        let known = self.known_origins(&authorization)?;
        let mut cursors = BTreeMap::new();
        let mut statement = self
            .db
            .connection()
            .prepare("SELECT origin_device_id, highest_contiguous_origin_seq FROM sync_cursors")?;
        for row in statement.query_map([], |row| {
            let id = fixed_bytes::<32>(row.get(0)?, 0)?;
            let value = row.get::<_, i64>(1)?;
            Ok((DeviceId::from_bytes(id), value))
        })? {
            let (origin, value) = row?;
            if !known.contains(&origin) {
                continue;
            }
            let value = u64::try_from(value).map_err(|_| ReplicationStoreError::InvalidCursor)?;
            if value > i64::MAX as u64 {
                return Err(ReplicationStoreError::InvalidCursor);
            }
            if value != 0 {
                cursors.insert(origin, value);
            }
        }
        Ok(cursors)
    }

    pub fn missing_changes(
        &self,
        remote: &BTreeMap<DeviceId, u64>,
        max_changes: usize,
        max_encoded_bytes: usize,
    ) -> Result<Vec<Change>, ReplicationStoreError> {
        if max_changes == 0
            || max_changes > MAX_CHANGES_PER_BATCH
            || max_encoded_bytes == 0
            || max_encoded_bytes > MAX_ENCODED_CHANGE_BYTES
        {
            return Err(ReplicationStoreError::BatchTooLarge);
        }
        let authorization = self.authorization_snapshot()?;
        let known = self.known_origins(&authorization)?;
        if remote.len() > MAX_CURSOR_ENTRIES || remote.keys().any(|id| !known.contains(id)) {
            return Err(ReplicationStoreError::UnknownOrigin);
        }
        for value in remote.values() {
            if *value > i64::MAX as u64 {
                return Err(ReplicationStoreError::InvalidCursor);
            }
        }

        let mut changes = Vec::new();
        let mut encoded_total = 0usize;
        for origin in known {
            let cursor = remote.get(&origin).copied().unwrap_or(0);
            if cursor > i64::MAX as u64 {
                return Err(ReplicationStoreError::InvalidCursor);
            }
            let mut statement = self.db.connection().prepare(
                "SELECT change_id, vault_id, item_id, parent_change_ids, origin_device_id,
                        origin_seq, hlc_physical_ms, hlc_logical, operation,
                        payload_schema_version, nonce, ciphertext, signature
                 FROM changes
                 WHERE origin_device_id = ?1 AND origin_seq > ?2
                 ORDER BY origin_seq ASC",
            )?;
            let rows = statement.query_map(
                params![origin.as_ref(), i64::try_from(cursor).unwrap_or(i64::MAX)],
                row_to_change,
            )?;
            for row in rows {
                let change = row?;
                let encoded = encode_change(&change)?;
                if encoded.len() > MAX_ENCODED_CHANGE_BYTES {
                    return Err(ReplicationStoreError::ChangeTooLarge);
                }
                if changes.len() == max_changes
                    || (encoded_total + encoded.len() > max_encoded_bytes && !changes.is_empty())
                {
                    return Ok(changes);
                }
                if encoded.len() > max_encoded_bytes && changes.is_empty() {
                    changes.push(change);
                    return Ok(changes);
                }
                encoded_total += encoded.len();
                changes.push(change);
                if changes.len() == max_changes {
                    return Ok(changes);
                }
            }
        }
        Ok(changes)
    }

    pub fn apply_remote_batch(
        &mut self,
        peer: DeviceId,
        authorization: &AuthorizationSnapshot,
        changes: &[Change],
    ) -> Result<BatchApplyResult, ReplicationStoreError> {
        if authorization.is_locally_blocked(peer) {
            return Err(ReplicationStoreError::BlockedAuthor);
        }
        authorization
            .authorize_peer(peer)
            .map_err(|_| ReplicationStoreError::UnknownOrigin)?;
        if authorization.membership.vault_id() != self.vault_id {
            return Err(ReplicationStoreError::WrongVault);
        }
        if changes.is_empty() || changes.len() > MAX_CHANGES_PER_BATCH {
            return Err(ReplicationStoreError::BatchTooLarge);
        }

        let mut canonical = Vec::with_capacity(changes.len());
        let mut total = 0usize;
        let mut by_id = BTreeMap::<ChangeId, Vec<u8>>::new();
        let mut by_origin = BTreeMap::<(DeviceId, u64), Vec<u8>>::new();
        let mut duplicates = 0usize;
        for original in changes {
            let change = original.canonicalized();
            if change.vault_id != self.vault_id {
                return Err(ReplicationStoreError::WrongVault);
            }
            change.validate()?;
            let signed = change.signed_bytes()?;
            let encoded_len = encode_change(&change)?.len();
            total = total
                .checked_add(encoded_len)
                .ok_or(ReplicationStoreError::BatchTooLarge)?;
            if total > MAX_BATCH_PLAINTEXT_BYTES
                && !(changes.len() == 1 && total <= MAX_ENCODED_CHANGE_BYTES)
            {
                return Err(ReplicationStoreError::BatchTooLarge);
            }
            if authorization.is_locally_blocked(change.origin_device_id) {
                return Err(ReplicationStoreError::BlockedAuthor);
            }
            let keys = authorization
                .authorize_peer(change.origin_device_id)
                .map_err(|_| ReplicationStoreError::UnknownOrigin)?;
            crate::crypto::keys::verify_signature(keys.ed25519, &signed, &change.signature)
                .map_err(|_| ReplicationStoreError::Journal(JournalError::InvalidSignature))?;
            if let Some(old) = by_id.insert(change.change_id, signed.clone()) {
                if old != signed {
                    return Err(ReplicationStoreError::Journal(
                        JournalError::ConflictingDuplicate,
                    ));
                }
                duplicates += 1;
                continue;
            }
            if let Some(old) =
                by_origin.insert((change.origin_device_id, change.origin_seq), signed)
            {
                if old != by_id[&change.change_id] {
                    return Err(ReplicationStoreError::Journal(
                        JournalError::ConflictingDuplicate,
                    ));
                }
                duplicates += 1;
                continue;
            }
            canonical.push(change);
        }

        let (inserted, transaction_duplicates) = self.db.transaction(|tx| {
            let peer_blocked: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM blocked_devices WHERE device_id = ?1)",
                [peer.as_ref()],
                |row| row.get(0),
            )?;
            if peer_blocked {
                return Err(ReplicationStoreError::BlockedAuthor);
            }
            for change in &canonical {
                let blocked: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM blocked_devices WHERE device_id = ?1)",
                    [change.origin_device_id.as_ref()],
                    |row| row.get(0),
                )?;
                if blocked {
                    return Err(ReplicationStoreError::BlockedAuthor);
                }
            }
            apply_changes_in_transaction(tx, &canonical).map_err(ReplicationStoreError::from)
        })?;
        let cursors_after_commit = self.cursor_map()?;
        Ok(BatchApplyResult {
            inserted,
            duplicates: duplicates + transaction_duplicates,
            cursors_after_commit,
        })
    }

    fn known_origins(
        &self,
        authorization: &AuthorizationSnapshot,
    ) -> Result<BTreeSet<DeviceId>, ReplicationStoreError> {
        let local = self.db.connection().query_row(
            "SELECT device_id FROM local_device WHERE singleton = 1",
            [],
            |row| fixed_bytes::<32>(row.get(0)?, 0),
        )?;
        let mut origins = authorization
            .membership
            .active_members()
            .map(|identity| identity.device_id)
            .collect::<BTreeSet<_>>();
        origins.insert(DeviceId::from_bytes(local));
        if origins.len() > MAX_CURSOR_ENTRIES {
            return Err(ReplicationStoreError::InvalidCursor);
        }
        Ok(origins)
    }
}

fn apply_changes_in_transaction(
    tx: &Transaction<'_>,
    changes: &[Change],
) -> Result<(usize, usize), JournalError> {
    let mut inserted = 0usize;
    let mut duplicates = 0usize;
    let mut items = BTreeSet::new();
    let mut origins = BTreeSet::new();
    let mut max_hlc = None;
    for change in changes {
        match check_duplicate(tx, change)? {
            Some(ApplyResult::Duplicate) => duplicates += 1,
            None => {
                insert_new_change(tx, change)?;
                inserted += 1;
                items.insert(change.item_id);
                origins.insert(change.origin_device_id);
                max_hlc = Some(max_hlc.map_or(change.hlc, |old: Hlc| old.max(change.hlc)));
            }
            Some(ApplyResult::Inserted) => unreachable!(),
        }
    }
    for item_id in items {
        merge::rebuild_item_projection(tx, item_id)?;
    }
    for origin in origins {
        advance_sync_cursor(tx, origin)?;
    }
    if let Some(hlc) = max_hlc {
        let vault_id = changes
            .first()
            .map(|change| change.vault_id)
            .ok_or(JournalError::InvalidChange("empty replication batch"))?;
        merge_remote_clock(tx, vault_id, hlc)?;
    }
    Ok((inserted, duplicates))
}

/// Encode one journal change using the bounded, deterministic replication wire format.
pub fn encode_change(change: &Change) -> Result<Vec<u8>, ReplicationStoreError> {
    let change = change.canonicalized();
    change.validate()?;
    let mut output = Vec::with_capacity(256 + change.ciphertext.len());
    output.extend_from_slice(CHANGE_WIRE_MAGIC);
    output.extend_from_slice(change.change_id.as_bytes());
    output.extend_from_slice(change.vault_id.as_bytes());
    output.extend_from_slice(change.item_id.as_bytes());
    output.extend_from_slice(change.origin_device_id.as_bytes());
    output.extend_from_slice(&change.origin_seq.to_le_bytes());
    output.extend_from_slice(&change.hlc.physical_ms.to_le_bytes());
    output.extend_from_slice(&change.hlc.logical.to_le_bytes());
    output.push(operation_tag(change.operation));
    output.extend_from_slice(&change.payload_schema_version.to_le_bytes());
    output.extend_from_slice(change.nonce.as_ref());
    let parents = u16::try_from(change.parent_change_ids.len())
        .map_err(|_| ReplicationStoreError::ChangeTooLarge)?;
    output.extend_from_slice(&parents.to_le_bytes());
    for parent in &change.parent_change_ids {
        output.extend_from_slice(parent.as_bytes());
    }
    let ciphertext_len = u32::try_from(change.ciphertext.len())
        .map_err(|_| ReplicationStoreError::ChangeTooLarge)?;
    output.extend_from_slice(&ciphertext_len.to_le_bytes());
    output.extend_from_slice(&change.ciphertext);
    output.extend_from_slice(&(change.signature.len() as u16).to_le_bytes());
    output.extend_from_slice(&change.signature);
    if output.len() > MAX_ENCODED_CHANGE_BYTES {
        return Err(ReplicationStoreError::ChangeTooLarge);
    }
    Ok(output)
}

/// Decode one bounded canonical journal change.
pub fn decode_change(bytes: &[u8]) -> Result<Change, ReplicationStoreError> {
    if bytes.len() > MAX_ENCODED_CHANGE_BYTES {
        return Err(ReplicationStoreError::ChangeTooLarge);
    }
    let mut cursor = WireCursor::new(bytes);
    if cursor.take(4)? != CHANGE_WIRE_MAGIC {
        return Err(ReplicationStoreError::InvalidEncoding);
    }
    let change_id = ChangeId::from_bytes(cursor.array()?);
    let vault_id = VaultId::from_bytes(cursor.array()?);
    let item_id = ItemId::from_bytes(cursor.array()?);
    let origin_device_id = DeviceId::from_bytes(cursor.array()?);
    let origin_seq = cursor.u64()?;
    let hlc = Hlc::new(cursor.u64()?, cursor.u32()?);
    let operation = match cursor.byte()? {
        0 => crate::crypto::Operation::Upsert,
        1 => crate::crypto::Operation::Tombstone,
        _ => return Err(ReplicationStoreError::InvalidEncoding),
    };
    let payload_schema_version = cursor.u32()?;
    let nonce = crate::crypto::Nonce::from_bytes(cursor.array()?);
    let parent_count = usize::from(cursor.u16()?);
    if parent_count > MAX_CURSOR_ENTRIES {
        return Err(ReplicationStoreError::InvalidEncoding);
    }
    let mut parent_change_ids = Vec::with_capacity(parent_count);
    for _ in 0..parent_count {
        parent_change_ids.push(ChangeId::from_bytes(cursor.array()?));
    }
    let ciphertext_length =
        usize::try_from(cursor.u32()?).map_err(|_| ReplicationStoreError::InvalidEncoding)?;
    if ciphertext_length > MAX_CHANGE_CIPHERTEXT_BYTES {
        return Err(ReplicationStoreError::ChangeTooLarge);
    }
    let ciphertext = cursor.take(ciphertext_length)?.to_vec();
    let signature_length = usize::from(cursor.u16()?);
    if signature_length != 64 {
        return Err(ReplicationStoreError::InvalidEncoding);
    }
    let signature = cursor.take(signature_length)?.to_vec();
    let change = Change {
        change_id,
        vault_id,
        item_id,
        parent_change_ids,
        origin_device_id,
        origin_seq,
        hlc,
        operation,
        payload_schema_version,
        nonce,
        ciphertext,
        signature,
    };
    if !cursor.is_empty() {
        return Err(ReplicationStoreError::InvalidEncoding);
    }
    change.validate()?;
    if change
        .parent_change_ids
        .windows(2)
        .any(|pair| pair[0] > pair[1])
    {
        return Err(ReplicationStoreError::InvalidEncoding);
    }
    Ok(change)
}

pub fn membership_set_hash(
    records: &[MembershipRecord],
) -> Result<[u8; 32], ReplicationStoreError> {
    let mut hashes = records
        .iter()
        .map(MembershipRecord::record_hash)
        .collect::<Result<Vec<MembershipRecordHash>, _>>()?;
    hashes.sort_unstable();
    let mut hasher = sha2::Sha256::new();
    hasher.update(b"NOX-REPLICATION-MEMBERSHIP\0");
    for hash in hashes {
        hasher.update(hash.as_bytes());
    }
    Ok(hasher.finalize().into())
}

struct WireCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> WireCursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], ReplicationStoreError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(ReplicationStoreError::InvalidEncoding)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(ReplicationStoreError::InvalidEncoding)?;
        self.offset = end;
        Ok(value)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], ReplicationStoreError> {
        self.take(N)?
            .try_into()
            .map_err(|_| ReplicationStoreError::InvalidEncoding)
    }

    fn byte(&mut self) -> Result<u8, ReplicationStoreError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, ReplicationStoreError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32, ReplicationStoreError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, ReplicationStoreError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

#[cfg(test)]
mod replication_tests {
    use super::*;
    use crate::crypto::{self, Ed25519Keypair, SecretKey};

    #[test]
    fn change_wire_round_trip_is_canonical_and_bounded() {
        let vault_id = VaultId::from_bytes([1; 16]);
        let item_id = ItemId::from_bytes([2; 16]);
        let signing_key = Ed25519Keypair::from_private_bytes([3; 32]);
        let encrypted = crypto::cipher::encrypt(
            &SecretKey::from_bytes([4; 32]),
            &crypto::AeadContext::new(
                vault_id,
                item_id,
                ChangeId::from_bytes([5; 16]),
                vec![ChangeId::from_bytes([8; 16]), ChangeId::from_bytes([7; 16])],
                DeviceId::from_public_key(signing_key.public_key_bytes()),
                1,
                Hlc::new(9, 1),
                crypto::Operation::Upsert,
                1,
            ),
            b"payload",
        )
        .unwrap();
        let change = Change::new_signed_with_id(
            ChangeId::from_bytes([5; 16]),
            vault_id,
            item_id,
            [ChangeId::from_bytes([8; 16]), ChangeId::from_bytes([7; 16])],
            DeviceId::from_public_key(signing_key.public_key_bytes()),
            1,
            Hlc::new(9, 1),
            crypto::Operation::Upsert,
            1,
            &encrypted,
            &signing_key,
        )
        .unwrap();
        let encoded = encode_change(&change).unwrap();
        let decoded = decode_change(&encoded).unwrap();
        assert_eq!(decoded.parent_change_ids, change.parent_change_ids);
        assert_eq!(
            decoded.signed_bytes().unwrap(),
            change.signed_bytes().unwrap()
        );
        assert!(matches!(
            decode_change(&encoded[..encoded.len() - 1]),
            Err(ReplicationStoreError::InvalidEncoding)
        ));
    }
}
