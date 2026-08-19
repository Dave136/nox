//! Immutable, signed, encrypted change journal operations.

use crate::{
    Hlc, HlcClock,
    crypto::{
        CipherError, Ed25519Keypair, EncryptedPayload, KeyError, Operation, SecretKey,
        cipher::{self, AeadContext, Nonce},
        keys,
    },
    ids::{ChangeId, DeviceId, ItemId, VaultId},
    merge,
    storage::Db,
};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params, types::Type};
use serde::{Deserialize, Serialize};
use std::{fmt, num::TryFromIntError};

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
            | Self::Encoding => None,
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
        if let Some(result) = check_duplicate(tx, &change)? {
            return Ok(result);
        }
        insert_new_change(tx, &change)?;
        merge::rebuild_item_projection(tx, change.item_id)?;
        advance_sync_cursor(tx, change.origin_device_id)?;
        merge_remote_clock(tx, change.vault_id, change.hlc)?;
        Ok(ApplyResult::Inserted)
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
