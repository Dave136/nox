//! Encrypted, bounded backup export and validated restore.

use crate::{
    Change, ChangeId, DeviceId, Ed25519Keypair, ItemId, JournalError, SecretKey, VaultId,
    X25519Keypair,
    crypto::{CipherError, Operation, cipher, keys},
    journal, merge,
    storage::Db,
};
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fmt, fs, io, path::Path};
use zeroize::Zeroizing;

const ARCHIVE_MAGIC: &[u8; 8] = b"LOCKBAK1";
const ARCHIVE_VERSION: u16 = 1;
const BACKUP_AAD: &[u8] = b"locker/backup/v1";
/// Maximum serialized backup size accepted before decryption or allocation.
pub const MAX_ARCHIVE_BYTES: usize = 32 * 1024 * 1024;
/// Maximum number of journal changes in one archive.
pub const MAX_CHANGES: usize = 100_000;
/// Maximum number of membership records in one archive.
pub const MAX_MEMBERSHIPS: usize = 100_000;
/// Maximum number of item projections in one archive.
pub const MAX_ITEMS: usize = 100_000;
const MAX_CIPHERTEXT_BYTES: usize = 8 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 1024 * 1024;

/// Errors returned by backup export and restore.
#[derive(Debug)]
pub enum BackupError {
    /// File I/O failed.
    Io(io::Error),
    /// SQLite rejected a restore operation.
    Sql(rusqlite::Error),
    /// Journal validation failed.
    Journal(JournalError),
    /// Archive encryption or decryption failed.
    Cipher(CipherError),
    /// The archive is malformed, unsupported, or exceeds a fixed limit.
    InvalidArchive(&'static str),
    /// No vault identifier could be found while exporting.
    MissingVaultId,
    /// A numeric archive value cannot be represented by SQLite.
    NumericOverflow,
}

impl fmt::Display for BackupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "backup I/O error: {error}"),
            Self::Sql(error) => write!(formatter, "backup SQLite error: {error}"),
            Self::Journal(error) => write!(formatter, "backup journal error: {error}"),
            Self::Cipher(error) => write!(formatter, "backup cipher error: {error}"),
            Self::InvalidArchive(reason) => write!(formatter, "invalid backup archive: {reason}"),
            Self::MissingVaultId => formatter.write_str("backup has no vault identifier"),
            Self::NumericOverflow => formatter.write_str("backup value exceeds SQLite range"),
        }
    }
}

impl std::error::Error for BackupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Sql(error) => Some(error),
            Self::Journal(error) => Some(error),
            Self::Cipher(error) => Some(error),
            Self::InvalidArchive(_) | Self::MissingVaultId | Self::NumericOverflow => None,
        }
    }
}

impl From<io::Error> for BackupError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rusqlite::Error> for BackupError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sql(error)
    }
}

impl From<JournalError> for BackupError {
    fn from(error: JournalError) -> Self {
        Self::Journal(error)
    }
}

impl From<CipherError> for BackupError {
    fn from(error: CipherError) -> Self {
        Self::Cipher(error)
    }
}

/// Restore outcome, including whether the archive required a fresh vault id.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RestoreResult {
    /// Vault id present after restore.
    pub vault_id: VaultId,
    /// Whether no membership survived and a new genesis was created.
    pub fresh_vault: bool,
    /// Number of journal changes present after restore.
    pub imported_changes: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ArchivePayload {
    version: u16,
    vault_id: VaultId,
    vault_meta: Option<VaultMetaArchive>,
    memberships: Vec<MembershipArchive>,
    changes: Vec<ArchiveChange>,
    items: Vec<ArchiveItem>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ArchiveChange {
    change: Change,
    signer_public_key: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ArchiveItem {
    item_id: ItemId,
    winning_change_id: ChangeId,
    deleted: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct MembershipArchive {
    record_hash: Vec<u8>,
    vault_id: VaultId,
    record_type: String,
    record: Vec<u8>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct VaultMetaArchive {
    format_version: i64,
    vault_id: VaultId,
    kdf_algorithm: String,
    kdf_salt: Vec<u8>,
    argon2_memory_kib: i64,
    argon2_iterations: i64,
    argon2_parallelism: i64,
    key_wrap_algorithm: String,
    wrapped_dek_nonce: Vec<u8>,
    wrapped_dek: Vec<u8>,
}

#[derive(Serialize)]
struct GenesisRecord {
    vault_id: VaultId,
    device_id: DeviceId,
    ed25519_public_key: [u8; 32],
}

/// Export the encrypted backup as bounded bytes.
pub fn export_backup(db: &Db, archive_key: &SecretKey) -> Result<Vec<u8>, BackupError> {
    let vault_id = find_vault_id(db.connection())?.ok_or(BackupError::MissingVaultId)?;
    let payload = ArchivePayload {
        version: ARCHIVE_VERSION,
        vault_id,
        vault_meta: load_vault_meta(db.connection(), vault_id)?,
        memberships: load_memberships(db.connection(), vault_id)?,
        changes: load_archive_changes(db.connection(), vault_id)?,
        items: load_items(db.connection())?,
    };
    validate_payload(&payload, archive_key)?;
    let plaintext = postcard::to_allocvec(&payload)
        .map_err(|_| BackupError::InvalidArchive("archive encoding failed"))?;
    if plaintext.len() > MAX_ARCHIVE_BYTES {
        return Err(BackupError::InvalidArchive("archive exceeds size limit"));
    }
    seal_archive(&plaintext, archive_key)
}

/// Restore an encrypted backup into an existing database atomically.
pub fn restore_backup(
    bytes: &[u8],
    db: &mut Db,
    archive_key: &SecretKey,
) -> Result<RestoreResult, BackupError> {
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(BackupError::InvalidArchive("archive exceeds size limit"));
    }
    let plaintext = open_archive(bytes, archive_key)?;
    if plaintext.len() > MAX_ARCHIVE_BYTES {
        return Err(BackupError::InvalidArchive(
            "archive payload exceeds size limit",
        ));
    }
    let payload: ArchivePayload = postcard::from_bytes(&plaintext)
        .map_err(|_| BackupError::InvalidArchive("archive decoding failed"))?;
    validate_payload(&payload, archive_key)?;
    let fresh_vault = payload.memberships.is_empty();
    let target_vault_id = if fresh_vault {
        VaultId::try_new().map_err(|_| BackupError::InvalidArchive("randomness unavailable"))?
    } else {
        payload.vault_id
    };
    let local_ed25519 = Ed25519Keypair::generate()
        .map_err(|_| BackupError::InvalidArchive("randomness unavailable"))?;
    let local_x25519 = X25519Keypair::generate()
        .map_err(|_| BackupError::InvalidArchive("randomness unavailable"))?;
    let private_material = Zeroizing::new(
        postcard::to_allocvec(&(
            local_ed25519.private_key_bytes(),
            local_x25519.private_key_bytes(),
        ))
        .map_err(|_| BackupError::InvalidArchive("private key encoding failed"))?,
    );
    let encrypted_private_material =
        seal_private_material(private_material.as_slice(), archive_key)?;
    let result = db.transaction(|tx| -> Result<RestoreResult, BackupError> {
        clear_database(tx)?;
        if fresh_vault {
            restore_fresh_vault(
                tx,
                &payload,
                target_vault_id,
                archive_key,
                &local_ed25519,
                &local_x25519,
                &encrypted_private_material,
            )?;
        } else {
            restore_existing_vault(
                tx,
                &payload,
                archive_key,
                &local_ed25519,
                &local_x25519,
                &encrypted_private_material,
            )?;
        }
        Ok(RestoreResult {
            vault_id: target_vault_id,
            fresh_vault,
            imported_changes: if fresh_vault {
                payload.items.len()
            } else {
                payload.changes.len()
            },
        })
    })?;
    Ok(result)
}

/// Export an encrypted archive to a temporary owner-only file and atomically rename it.
pub fn export_to_path(
    db: &Db,
    archive_key: &SecretKey,
    path: impl AsRef<Path>,
) -> Result<(), BackupError> {
    let path = path.as_ref();
    let bytes = export_backup(db, archive_key)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let suffix =
        getrandom::u32().map_err(|_| BackupError::InvalidArchive("randomness unavailable"))?;
    let temporary = path.with_extension(format!("tmp-{suffix:08x}"));
    write_owner_only(&temporary, &bytes)?;
    let file = fs::OpenOptions::new().write(true).open(&temporary)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temporary, path)?;
    Ok(())
}

/// Restore an encrypted archive from a bounded file.
pub fn restore_from_path(
    path: impl AsRef<Path>,
    db: &mut Db,
    archive_key: &SecretKey,
) -> Result<RestoreResult, BackupError> {
    let metadata = fs::metadata(path.as_ref())?;
    if metadata.len() > MAX_ARCHIVE_BYTES as u64 {
        return Err(BackupError::InvalidArchive("archive exceeds size limit"));
    }
    restore_backup(&fs::read(path)?, db, archive_key)
}

/// Short aliases for callers that use export/restore terminology.
pub use export_backup as export;
/// Short aliases for callers that use export/restore terminology.
pub use restore_backup as restore;

fn seal_archive(plaintext: &[u8], key: &SecretKey) -> Result<Vec<u8>, BackupError> {
    let mut nonce = [0_u8; 24];
    getrandom::fill(&mut nonce)
        .map_err(|_| BackupError::InvalidArchive("randomness unavailable"))?;
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_bytes())
        .map_err(|_| BackupError::InvalidArchive("invalid archive key"))?;
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: BACKUP_AAD,
            },
        )
        .map_err(|_| BackupError::InvalidArchive("archive encryption failed"))?;
    if ciphertext.len() + 33 > MAX_ARCHIVE_BYTES {
        return Err(BackupError::InvalidArchive("archive exceeds size limit"));
    }
    let mut output = Vec::with_capacity(8 + 1 + 24 + ciphertext.len());
    output.extend_from_slice(ARCHIVE_MAGIC);
    output.push(ARCHIVE_VERSION as u8);
    output.extend_from_slice(&nonce);
    output.extend_from_slice(&ciphertext);
    Ok(output)
}

fn open_archive(bytes: &[u8], key: &SecretKey) -> Result<Vec<u8>, BackupError> {
    if bytes.len() < 8 + 1 + 24 + 16
        || &bytes[..8] != ARCHIVE_MAGIC
        || bytes[8] != ARCHIVE_VERSION as u8
    {
        return Err(BackupError::InvalidArchive("invalid archive header"));
    }
    let nonce: [u8; 24] = bytes[9..33]
        .try_into()
        .map_err(|_| BackupError::InvalidArchive("invalid archive nonce"))?;
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_bytes())
        .map_err(|_| BackupError::InvalidArchive("invalid archive key"))?;
    cipher
        .decrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &bytes[33..],
                aad: BACKUP_AAD,
            },
        )
        .map_err(|_| BackupError::InvalidArchive("archive authentication failed"))
}

fn seal_private_material(plaintext: &[u8], key: &SecretKey) -> Result<Vec<u8>, BackupError> {
    keys::seal_fixed_aad(key, keys::PRIVATE_KEY_AAD, plaintext)
        .map_err(|_| BackupError::InvalidArchive("private key encryption failed"))
}

fn validate_payload(payload: &ArchivePayload, key: &SecretKey) -> Result<(), BackupError> {
    if payload.version != ARCHIVE_VERSION {
        return Err(BackupError::InvalidArchive("unsupported archive version"));
    }
    if payload.memberships.len() > MAX_MEMBERSHIPS
        || payload.changes.len() > MAX_CHANGES
        || payload.items.len() > MAX_ITEMS
    {
        return Err(BackupError::InvalidArchive("archive record limit exceeded"));
    }
    if let Some(meta) = &payload.vault_meta {
        if meta.vault_id != payload.vault_id {
            return Err(BackupError::InvalidArchive("vault header id mismatch"));
        }
        if meta.kdf_salt.len() > MAX_RECORD_BYTES
            || meta.wrapped_dek_nonce.len() > MAX_RECORD_BYTES
            || meta.wrapped_dek.len() > MAX_RECORD_BYTES
        {
            return Err(BackupError::InvalidArchive("vault header field too large"));
        }
    }
    let mut membership_hashes = std::collections::HashSet::new();
    for membership in &payload.memberships {
        if membership.vault_id != payload.vault_id
            || membership.record_hash.len() != 32
            || membership.record_type.is_empty()
            || membership.record.len() > MAX_RECORD_BYTES
            || !membership_hashes.insert(membership.record_hash.clone())
        {
            return Err(BackupError::InvalidArchive("invalid membership record"));
        }
        let digest = Sha256::digest(&membership.record);
        if digest.as_slice() != membership.record_hash.as_slice() {
            return Err(BackupError::InvalidArchive("membership hash mismatch"));
        }
    }
    let mut change_ids = std::collections::HashSet::new();
    let mut origin_sequences = std::collections::HashSet::new();
    for archived in &payload.changes {
        let change = &archived.change;
        if change.vault_id != payload.vault_id
            || change.signature.len() != 64
            || change.ciphertext.len() < 16
            || change.ciphertext.len() > MAX_CIPHERTEXT_BYTES
            || !change_ids.insert(change.change_id)
            || !origin_sequences.insert((change.origin_device_id, change.origin_seq))
        {
            return Err(BackupError::InvalidArchive("invalid journal record"));
        }
        if let Some(public_key) = &archived.signer_public_key {
            keys::verify_signature(public_key, &change.signed_bytes()?, &change.signature)
                .map_err(|_| BackupError::InvalidArchive("journal signature mismatch"))?;
        }
        cipher::decrypt(key, &change.aad_context(), &change.encrypted_payload())
            .map_err(|_| BackupError::InvalidArchive("journal payload authentication failed"))?;
    }
    for item in &payload.items {
        let Some(winner) = payload
            .changes
            .iter()
            .find(|archived| archived.change.change_id == item.winning_change_id)
        else {
            return Err(BackupError::InvalidArchive("item winner is missing"));
        };
        if winner.change.item_id != item.item_id {
            return Err(BackupError::InvalidArchive(
                "item winner belongs to another item",
            ));
        }
    }
    Ok(())
}

fn find_vault_id(connection: &Connection) -> Result<Option<VaultId>, BackupError> {
    let from_header = connection
        .query_row("SELECT vault_id FROM vault_meta LIMIT 1", [], |row| {
            row.get::<_, Vec<u8>>(0)
        })
        .optional()?;
    if let Some(bytes) = from_header {
        return Ok(Some(VaultId::from_bytes(
            bytes
                .try_into()
                .map_err(|_| BackupError::InvalidArchive("invalid vault id"))?,
        )));
    }
    let from_change = connection
        .query_row("SELECT vault_id FROM changes LIMIT 1", [], |row| {
            row.get::<_, Vec<u8>>(0)
        })
        .optional()?;
    match from_change {
        Some(bytes) => {
            Ok(Some(VaultId::from_bytes(bytes.try_into().map_err(
                |_| BackupError::InvalidArchive("invalid vault id"),
            )?)))
        }
        None => Ok(None),
    }
}

fn load_vault_meta(
    connection: &Connection,
    vault_id: VaultId,
) -> Result<Option<VaultMetaArchive>, BackupError> {
    connection
        .query_row(
            "SELECT format_version, vault_id, kdf_algorithm, kdf_salt,
                    argon2_memory_kib, argon2_iterations, argon2_parallelism,
                    key_wrap_algorithm, wrapped_dek_nonce, wrapped_dek
             FROM vault_meta WHERE vault_id = ?1 LIMIT 1",
            [vault_id.as_ref()],
            |row| {
                Ok(VaultMetaArchive {
                    format_version: row.get(0)?,
                    vault_id: VaultId::from_bytes(
                        row.get::<_, Vec<u8>>(1)?
                            .try_into()
                            .map_err(|_| rusqlite::Error::InvalidQuery)?,
                    ),
                    kdf_algorithm: row.get(2)?,
                    kdf_salt: row.get(3)?,
                    argon2_memory_kib: row.get(4)?,
                    argon2_iterations: row.get(5)?,
                    argon2_parallelism: row.get(6)?,
                    key_wrap_algorithm: row.get(7)?,
                    wrapped_dek_nonce: row.get(8)?,
                    wrapped_dek: row.get(9)?,
                })
            },
        )
        .optional()
        .map_err(BackupError::from)
}

fn load_memberships(
    connection: &Connection,
    vault_id: VaultId,
) -> Result<Vec<MembershipArchive>, BackupError> {
    let mut statement = connection.prepare(
        "SELECT record_hash, vault_id, record_type, record FROM memberships WHERE vault_id = ?1",
    )?;
    Ok(statement
        .query_map([vault_id.as_ref()], |row| {
            Ok(MembershipArchive {
                record_hash: row.get(0)?,
                vault_id: VaultId::from_bytes(
                    row.get::<_, Vec<u8>>(1)?
                        .try_into()
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                ),
                record_type: row.get(2)?,
                record: row.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?)
}

fn load_archive_changes(
    connection: &Connection,
    vault_id: VaultId,
) -> Result<Vec<ArchiveChange>, BackupError> {
    let changes = journal::load_all_changes(connection, vault_id)?;
    let local_public_key = connection
        .query_row(
            "SELECT ed25519_public_key FROM local_device WHERE singleton = 1",
            [],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?;
    Ok(changes
        .into_iter()
        .map(|change| {
            let signer_public_key = local_public_key
                .clone()
                .filter(|key| key.len() == 32)
                .filter(|key| DeviceId::from_public_key(key.as_slice()) == change.origin_device_id);
            ArchiveChange {
                signer_public_key,
                change,
            }
        })
        .collect())
}

fn load_items(connection: &Connection) -> Result<Vec<ArchiveItem>, BackupError> {
    let mut statement =
        connection.prepare("SELECT item_id, winning_change_id, deleted FROM items")?;
    Ok(statement
        .query_map([], |row| {
            Ok(ArchiveItem {
                item_id: ItemId::from_bytes(
                    row.get::<_, Vec<u8>>(0)?
                        .try_into()
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                ),
                winning_change_id: crate::ChangeId::from_bytes(
                    row.get::<_, Vec<u8>>(1)?
                        .try_into()
                        .map_err(|_| rusqlite::Error::InvalidQuery)?,
                ),
                deleted: row.get::<_, i64>(2)? != 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?)
}

fn clear_database(tx: &Transaction<'_>) -> Result<(), BackupError> {
    tx.execute_batch(
        "DELETE FROM conflicts;
         DELETE FROM items;
         DELETE FROM changes;
         DELETE FROM sync_cursors;
         DELETE FROM clock_state;
         DELETE FROM memberships;
         DELETE FROM local_device;
         DELETE FROM vault_meta;",
    )?;
    Ok(())
}

fn restore_existing_vault(
    tx: &Transaction<'_>,
    payload: &ArchivePayload,
    key: &SecretKey,
    local_ed25519: &Ed25519Keypair,
    local_x25519: &X25519Keypair,
    encrypted_private_material: &[u8],
) -> Result<(), BackupError> {
    if let Some(meta) = &payload.vault_meta {
        insert_vault_meta(tx, meta)?;
    }
    insert_memberships(tx, &payload.memberships)?;
    insert_changes(tx, &payload.changes)?;
    rebuild_items(tx, &payload.changes)?;
    insert_local_device(tx, local_ed25519, local_x25519, encrypted_private_material)?;
    insert_clock_state_from_changes(tx, &payload.changes)?;
    let _ = key;
    Ok(())
}

fn restore_fresh_vault(
    tx: &Transaction<'_>,
    payload: &ArchivePayload,
    vault_id: VaultId,
    key: &SecretKey,
    local_ed25519: &Ed25519Keypair,
    local_x25519: &X25519Keypair,
    encrypted_private_material: &[u8],
) -> Result<(), BackupError> {
    if let Some(meta) = &payload.vault_meta {
        let mut fresh_meta = meta.clone();
        fresh_meta.vault_id = vault_id;
        insert_vault_meta(tx, &fresh_meta)?;
    }
    let device_id = DeviceId::from_public_key(local_ed25519.public_key_bytes());
    let genesis = postcard::to_allocvec(&GenesisRecord {
        vault_id,
        device_id,
        ed25519_public_key: local_ed25519.public_key_bytes(),
    })
    .map_err(|_| BackupError::InvalidArchive("genesis encoding failed"))?;
    tx.execute(
        "INSERT INTO memberships (record_hash, vault_id, record_type, record) VALUES (?1, ?2, 'genesis', ?3)",
        params![Sha256::digest(&genesis).as_slice(), vault_id.as_ref(), genesis],
    )?;
    let mut origin_seq = 0_u64;
    for item in &payload.items {
        let source = payload
            .changes
            .iter()
            .find(|archived| archived.change.change_id == item.winning_change_id)
            .ok_or(BackupError::InvalidArchive("item winner is missing"))?;
        let plaintext = cipher::decrypt(
            key,
            &source.change.aad_context(),
            &source.change.encrypted_payload(),
        )?;
        origin_seq = origin_seq
            .checked_add(1)
            .ok_or(BackupError::NumericOverflow)?;
        let change_id = crate::ChangeId::new();
        let context = crate::AeadContext::new(
            vault_id,
            item.item_id,
            change_id,
            [],
            device_id,
            origin_seq,
            source.change.hlc,
            source.change.operation,
            source.change.payload_schema_version,
        );
        let encrypted = if matches!(source.change.operation, Operation::Tombstone) {
            cipher::encrypt_tombstone(key, &context)?
        } else {
            cipher::encrypt(key, &context, plaintext.as_bytes())?
        };
        let fresh_change = Change::new_signed_with_id(
            change_id,
            vault_id,
            item.item_id,
            [],
            device_id,
            origin_seq,
            source.change.hlc,
            source.change.operation,
            source.change.payload_schema_version,
            &encrypted,
            local_ed25519,
        )?;
        journal::insert_new_change(tx, &fresh_change)?;
        merge::rebuild_item_projection(tx, item.item_id)?;
    }
    insert_local_device(tx, local_ed25519, local_x25519, encrypted_private_material)?;
    let max_physical_ms = payload
        .items
        .iter()
        .filter_map(|item| {
            payload
                .changes
                .iter()
                .find(|change| change.change.change_id == item.winning_change_id)
                .map(|change| change.change.hlc.physical_ms)
        })
        .max()
        .unwrap_or(0);
    tx.execute(
        "INSERT INTO clock_state (vault_id, hlc_physical_ms, hlc_logical, next_origin_seq)
         VALUES (?1, ?2, 0, ?3)",
        params![
            vault_id.as_ref(),
            i64::try_from(max_physical_ms).map_err(|_| BackupError::NumericOverflow)?,
            i64::try_from(origin_seq).map_err(|_| BackupError::NumericOverflow)?,
        ],
    )?;
    Ok(())
}

fn insert_vault_meta(tx: &Transaction<'_>, meta: &VaultMetaArchive) -> Result<(), BackupError> {
    tx.execute(
        "INSERT INTO vault_meta (singleton, format_version, vault_id, kdf_algorithm, kdf_salt,
            argon2_memory_kib, argon2_iterations, argon2_parallelism, key_wrap_algorithm,
            wrapped_dek_nonce, wrapped_dek)
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            meta.format_version,
            meta.vault_id.as_ref(),
            meta.kdf_algorithm,
            meta.kdf_salt,
            meta.argon2_memory_kib,
            meta.argon2_iterations,
            meta.argon2_parallelism,
            meta.key_wrap_algorithm,
            meta.wrapped_dek_nonce,
            meta.wrapped_dek,
        ],
    )?;
    Ok(())
}

fn insert_memberships(
    tx: &Transaction<'_>,
    memberships: &[MembershipArchive],
) -> Result<(), BackupError> {
    for membership in memberships {
        tx.execute(
            "INSERT INTO memberships (record_hash, vault_id, record_type, record) VALUES (?1, ?2, ?3, ?4)",
            params![membership.record_hash, membership.vault_id.as_ref(), membership.record_type, membership.record],
        )?;
    }
    Ok(())
}

fn insert_changes(tx: &Transaction<'_>, changes: &[ArchiveChange]) -> Result<(), BackupError> {
    for archived in changes {
        journal::insert_new_change(tx, &archived.change)?;
    }
    Ok(())
}

fn rebuild_items(tx: &Transaction<'_>, changes: &[ArchiveChange]) -> Result<(), BackupError> {
    let mut item_ids = std::collections::HashSet::new();
    for archived in changes {
        if item_ids.insert(archived.change.item_id) {
            merge::rebuild_item_projection(tx, archived.change.item_id)?;
        }
    }
    Ok(())
}

fn insert_local_device(
    tx: &Transaction<'_>,
    ed25519: &Ed25519Keypair,
    x25519: &X25519Keypair,
    encrypted_private_material: &[u8],
) -> Result<(), BackupError> {
    let device_id = DeviceId::from_public_key(ed25519.public_key_bytes());
    tx.execute(
        "INSERT INTO local_device (singleton, device_id, ed25519_public_key, x25519_public_key, encrypted_private_key_material)
         VALUES (1, ?1, ?2, ?3, ?4)",
        params![
            device_id.as_ref(),
            ed25519.public_key_bytes().as_slice(),
            x25519.public_key_bytes().as_slice(),
            encrypted_private_material,
        ],
    )?;
    Ok(())
}

fn insert_clock_state_from_changes(
    tx: &Transaction<'_>,
    changes: &[ArchiveChange],
) -> Result<(), BackupError> {
    let (physical_ms, logical) = changes.iter().fold((0_u64, 0_u32), |current, archived| {
        current.max((archived.change.hlc.physical_ms, archived.change.hlc.logical))
    });
    let vault_id = changes.first().map(|change| change.change.vault_id);
    if let Some(vault_id) = vault_id {
        tx.execute(
            "INSERT INTO clock_state (vault_id, hlc_physical_ms, hlc_logical, next_origin_seq) VALUES (?1, ?2, ?3, 0)",
            params![vault_id.as_ref(), i64::try_from(physical_ms).map_err(|_| BackupError::NumericOverflow)?, i64::from(logical)],
        )?;
    }
    Ok(())
}

fn write_owner_only(path: &Path, bytes: &[u8]) -> Result<(), BackupError> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    use io::Write;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
