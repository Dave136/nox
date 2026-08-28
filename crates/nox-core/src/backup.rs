//! Encrypted, bounded backup export and validated restore.

use crate::{
    Change, ChangeId, DeviceId, Ed25519Keypair, ItemId, JournalError, SecretKey, VaultId,
    X25519Keypair,
    crypto::secret::SecretBytes,
    crypto::{CipherError, Operation, cipher, kdf, keys},
    journal,
    membership::{self, MembershipRecord},
    merge,
    storage::Db,
};
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

const ARCHIVE_MAGIC: &[u8; 8] = b"NOXBACK1";
const ARCHIVE_VERSION: u16 = 1;
const BACKUP_AAD: &[u8] = b"nox/backup/v1";
const V2_MAGIC: &[u8; 8] = b"NOXBACK2";
const V2_VERSION: u16 = 2;
const V2_KDF_ARGON2ID: u8 = 1;
const V2_WRAP_AAD: &[u8] = b"nox/backup/v2/wrap";
/// Maximum serialized backup size accepted before decryption or allocation.
pub const MAX_ARCHIVE_BYTES: usize = 32 * 1024 * 1024;
/// Maximum backup-password input accepted by the v2 recovery container.
pub const MAX_BACKUP_PASSWORD_BYTES: usize = 1024;
/// Maximum number of journal changes in one archive.
pub const MAX_CHANGES: usize = 100_000;
/// Maximum number of membership records in one archive.
pub const MAX_MEMBERSHIPS: usize = 100_000;
/// Maximum number of item projections in one archive.
pub const MAX_ITEMS: usize = 100_000;
const MAX_RECORD_BYTES: usize = 1024 * 1024;

/// Errors returned by backup export and restore.
pub enum BackupError {
    /// File I/O failed.
    Io(io::Error),
    /// SQLite rejected a restore operation.
    Sql(rusqlite::Error),
    /// Journal validation failed.
    Journal(JournalError),
    /// Backup password or authenticated v2 archive material was invalid.
    AuthenticationFailed,
    /// The archive container version is unsupported.
    UnsupportedVersion,
    /// The source vault is not suitable for a backup request.
    UnsupportedSource,
    /// Archive and destination resolve to the same file.
    SourceEqualsDestination,
    /// Password KDF failed.
    Kdf(kdf::KdfError),
    /// Key wrapping failed.
    Key(crate::crypto::keys::KeyError),
    /// The archive is malformed, unsupported, or exceeds a fixed limit.
    InvalidArchive(&'static str),
    /// No vault identifier could be found while exporting.
    MissingVaultId,
    /// A numeric archive value cannot be represented by SQLite.
    NumericOverflow,
}

impl fmt::Debug for BackupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(_) => formatter.write_str("BackupError::Io"),
            Self::Sql(_) => formatter.write_str("BackupError::Sql"),
            Self::Journal(_) => formatter.write_str("BackupError::Journal"),
            Self::AuthenticationFailed => formatter.write_str("BackupError::AuthenticationFailed"),
            Self::UnsupportedVersion => formatter.write_str("BackupError::UnsupportedVersion"),
            Self::UnsupportedSource => formatter.write_str("BackupError::UnsupportedSource"),
            Self::SourceEqualsDestination => {
                formatter.write_str("BackupError::SourceEqualsDestination")
            }
            Self::Kdf(_) => formatter.write_str("BackupError::Kdf"),
            Self::Key(_) => formatter.write_str("BackupError::Key"),
            Self::InvalidArchive(_) => formatter.write_str("BackupError::InvalidArchive"),
            Self::MissingVaultId => formatter.write_str("BackupError::MissingVaultId"),
            Self::NumericOverflow => formatter.write_str("BackupError::NumericOverflow"),
        }
    }
}

impl fmt::Display for BackupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(_) => formatter.write_str("backup I/O error"),
            Self::Sql(_) => formatter.write_str("backup SQLite error"),
            Self::Journal(_) => formatter.write_str("backup journal error"),
            Self::AuthenticationFailed => formatter.write_str("backup authentication failed"),
            Self::UnsupportedVersion => formatter.write_str("unsupported backup version"),
            Self::UnsupportedSource => formatter.write_str("unsupported backup source"),
            Self::SourceEqualsDestination => {
                formatter.write_str("backup source equals destination")
            }
            Self::Kdf(_) => formatter.write_str("backup KDF error"),
            Self::Key(_) => formatter.write_str("backup key error"),
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
            Self::AuthenticationFailed
            | Self::UnsupportedVersion
            | Self::UnsupportedSource
            | Self::SourceEqualsDestination
            | Self::InvalidArchive(_)
            | Self::MissingVaultId
            | Self::NumericOverflow => None,
            Self::Kdf(error) => Some(error),
            Self::Key(error) => Some(error),
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
    fn from(_: CipherError) -> Self {
        Self::InvalidArchive("archive authentication failed")
    }
}

impl From<kdf::KdfError> for BackupError {
    fn from(error: kdf::KdfError) -> Self {
        Self::Kdf(error)
    }
}

impl From<crate::crypto::keys::KeyError> for BackupError {
    fn from(error: crate::crypto::keys::KeyError) -> Self {
        Self::Key(error)
    }
}

/// Opaque, secret-owning v2 backup export request.
pub struct BackupExportRequest {
    source: PathBuf,
    destination: PathBuf,
    backup_password: SecretBytes,
    dek: SecretKey,
}

impl fmt::Debug for BackupExportRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BackupExportRequest(<redacted>)")
    }
}

impl BackupExportRequest {
    pub(crate) fn new(
        source: impl AsRef<Path>,
        backup_password: &[u8],
        destination: impl AsRef<Path>,
        dek: &SecretKey,
    ) -> Result<Self, BackupError> {
        if backup_password.is_empty() || backup_password.len() > MAX_BACKUP_PASSWORD_BYTES {
            return Err(BackupError::InvalidArchive("invalid backup password"));
        }
        let source = source.as_ref().to_path_buf();
        let destination = destination.as_ref().to_path_buf();
        if paths_equal(&source, &destination) {
            return Err(BackupError::SourceEqualsDestination);
        }
        Ok(Self {
            source,
            destination,
            backup_password: SecretBytes::new(backup_password),
            dek: dek.clone(),
        })
    }

    /// Run the bounded snapshot, encryption, and atomic output operation.
    pub fn run(self) -> Result<(), BackupError> {
        if paths_equal(&self.source, &self.destination) {
            return Err(BackupError::SourceEqualsDestination);
        }
        let mut db = Db::open(&self.source).map_err(|error| match error {
            crate::storage::DbError::Sql(error) => BackupError::Sql(error),
            crate::storage::DbError::Io(error) => BackupError::Io(error),
            crate::storage::DbError::Migration(_) => {
                BackupError::InvalidArchive("database migration failed")
            }
        })?;
        export_v2_to_path(
            &mut db,
            &self.dek,
            self.backup_password.as_bytes(),
            &self.destination,
        )
    }
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    let normalize = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    normalize(left) == normalize(right)
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

#[derive(Deserialize, Serialize)]
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
    let fresh_vault = fresh_membership_recovery(&payload);
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
                payload.vault_meta.as_ref(),
            )?;
        } else {
            restore_existing_vault(
                tx,
                &payload,
                archive_key,
                &local_ed25519,
                &local_x25519,
                &encrypted_private_material,
                payload.vault_meta.as_ref(),
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
    write_owner_only_atomic(path, &bytes)
}

fn export_v2_to_path(
    db: &mut Db,
    dek: &SecretKey,
    backup_password: &[u8],
    path: &Path,
) -> Result<(), BackupError> {
    // Keep the SQLite snapshot open only while materializing bounded rows.
    let payload = db.transaction(|tx| {
        let connection: &Connection = tx;
        let vault_id = find_vault_id(connection)?.ok_or(BackupError::MissingVaultId)?;
        Ok::<_, BackupError>(ArchivePayload {
            version: ARCHIVE_VERSION,
            vault_id,
            vault_meta: None,
            memberships: load_memberships(connection, vault_id)?,
            changes: load_archive_changes(connection, vault_id)?,
            items: load_items(connection)?,
        })
    })?;
    validate_payload(&payload, dek)?;
    let plaintext = postcard::to_allocvec(&payload)
        .map_err(|_| BackupError::InvalidArchive("archive encoding failed"))?;
    if plaintext.len() > MAX_ARCHIVE_BYTES {
        return Err(BackupError::InvalidArchive("archive exceeds size limit"));
    }
    let salt =
        kdf::random_salt().map_err(|_| BackupError::InvalidArchive("randomness unavailable"))?;
    let params = kdf::Argon2Params::v1();
    let kek = kdf::derive_kek(backup_password, salt, params)?;
    let (wrapped_nonce, wrapped_dek) = wrap_backup_dek(&kek, dek)?;
    let mut archive_nonce = [0_u8; 24];
    getrandom::fill(&mut archive_nonce)
        .map_err(|_| BackupError::InvalidArchive("randomness unavailable"))?;
    let ciphertext_len = plaintext
        .len()
        .checked_add(16)
        .ok_or(BackupError::NumericOverflow)?;
    if ciphertext_len > u32::MAX as usize {
        return Err(BackupError::NumericOverflow);
    }
    let header = encode_v2_header(
        salt,
        params,
        wrapped_nonce,
        &wrapped_dek,
        archive_nonce,
        ciphertext_len as u32,
    )?;
    let cipher = XChaCha20Poly1305::new_from_slice(dek.as_bytes())
        .map_err(|_| BackupError::InvalidArchive("invalid archive key"))?;
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&archive_nonce),
            Payload {
                msg: &plaintext,
                aad: &header,
            },
        )
        .map_err(|_| BackupError::InvalidArchive("archive encryption failed"))?;
    if ciphertext.len() != ciphertext_len {
        return Err(BackupError::InvalidArchive("archive length mismatch"));
    }
    let mut bytes = header;
    bytes.extend_from_slice(&ciphertext);
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(BackupError::InvalidArchive("archive exceeds size limit"));
    }
    write_owner_only_atomic(path, &bytes)
}

fn wrap_backup_dek(kek: &SecretKey, dek: &SecretKey) -> Result<([u8; 24], Vec<u8>), BackupError> {
    let mut nonce = [0_u8; 24];
    getrandom::fill(&mut nonce)
        .map_err(|_| BackupError::InvalidArchive("randomness unavailable"))?;
    let cipher = XChaCha20Poly1305::new_from_slice(kek.as_bytes())
        .map_err(|_| BackupError::InvalidArchive("invalid backup key"))?;
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: dek.as_bytes(),
                aad: V2_WRAP_AAD,
            },
        )
        .map_err(|_| BackupError::InvalidArchive("backup key wrapping failed"))?;
    Ok((nonce, ciphertext))
}

fn encode_v2_header(
    salt: [u8; 16],
    params: kdf::Argon2Params,
    wrapped_nonce: [u8; 24],
    wrapped_dek: &[u8],
    archive_nonce: [u8; 24],
    ciphertext_len: u32,
) -> Result<Vec<u8>, BackupError> {
    if wrapped_dek.len() != 48 || wrapped_dek.len() > u16::MAX as usize {
        return Err(BackupError::InvalidArchive("invalid wrapped key length"));
    }
    let mut header = Vec::with_capacity(141);
    header.extend_from_slice(V2_MAGIC);
    header.extend_from_slice(&V2_VERSION.to_le_bytes());
    header.push(V2_KDF_ARGON2ID);
    header.extend_from_slice(&salt);
    header.extend_from_slice(&params.memory_kib.to_le_bytes());
    header.extend_from_slice(&params.iterations.to_le_bytes());
    header.extend_from_slice(&params.parallelism.to_le_bytes());
    header.extend_from_slice(&wrapped_nonce);
    header.extend_from_slice(&(wrapped_dek.len() as u16).to_le_bytes());
    header.extend_from_slice(wrapped_dek);
    header.extend_from_slice(&archive_nonce);
    header.extend_from_slice(&ciphertext_len.to_le_bytes());
    Ok(header)
}

fn read_v2_archive(
    bytes: &[u8],
    backup_password: &[u8],
) -> Result<(ArchivePayload, SecretKey), BackupError> {
    if backup_password.is_empty() || backup_password.len() > MAX_BACKUP_PASSWORD_BYTES {
        return Err(BackupError::AuthenticationFailed);
    }
    const FIXED_PREFIX: usize = 8 + 2 + 1 + 16 + 4 + 4 + 4 + 24 + 2;
    if bytes.len() < 8 {
        return Err(BackupError::InvalidArchive("archive header is truncated"));
    }
    if bytes.get(..8) == Some(ARCHIVE_MAGIC.as_slice()) {
        return Err(BackupError::UnsupportedVersion);
    }
    if bytes.len() < FIXED_PREFIX {
        return Err(BackupError::InvalidArchive("archive header is truncated"));
    }
    let mut offset = 0;
    let magic = take_v2_array::<8>(bytes, &mut offset)?;
    if magic != *V2_MAGIC {
        return Err(BackupError::InvalidArchive("invalid archive magic"));
    }
    let version = u16::from_le_bytes(take_v2_array(bytes, &mut offset)?);
    if version != V2_VERSION {
        return Err(BackupError::UnsupportedVersion);
    }
    let [kdf_algorithm] = take_v2_array(bytes, &mut offset)?;
    if kdf_algorithm != V2_KDF_ARGON2ID {
        return Err(BackupError::UnsupportedVersion);
    }
    let salt: [u8; 16] = take_v2_array(bytes, &mut offset)?;
    let memory = u32::from_le_bytes(take_v2_array(bytes, &mut offset)?);
    let iterations = u32::from_le_bytes(take_v2_array(bytes, &mut offset)?);
    let parallelism = u32::from_le_bytes(take_v2_array(bytes, &mut offset)?);
    let params = kdf::Argon2Params::new(memory, iterations, parallelism);
    if params != kdf::Argon2Params::v1() {
        return Err(BackupError::InvalidArchive("unsupported KDF parameters"));
    }
    let wrapped_nonce: [u8; 24] = take_v2_array(bytes, &mut offset)?;
    let wrapped_len = u16::from_le_bytes(take_v2_array(bytes, &mut offset)?) as usize;
    if wrapped_len != 48 {
        return Err(BackupError::InvalidArchive("invalid wrapped key length"));
    }
    let wrapped = take_v2_bytes(bytes, &mut offset, wrapped_len)?;
    let archive_nonce: [u8; 24] = take_v2_array(bytes, &mut offset)?;
    let ciphertext_len = u32::from_le_bytes(take_v2_array(bytes, &mut offset)?) as usize;
    if !(16..=MAX_ARCHIVE_BYTES).contains(&ciphertext_len) {
        return Err(BackupError::InvalidArchive("invalid ciphertext length"));
    }
    let end = offset
        .checked_add(ciphertext_len)
        .ok_or(BackupError::NumericOverflow)?;
    if end != bytes.len() {
        return Err(BackupError::InvalidArchive("archive length mismatch"));
    }
    let kek = kdf::derive_kek(backup_password, salt, params)
        .map_err(|_| BackupError::AuthenticationFailed)?;
    let kek_cipher = XChaCha20Poly1305::new_from_slice(kek.as_bytes())
        .map_err(|_| BackupError::AuthenticationFailed)?;
    let dek_bytes = kek_cipher
        .decrypt(
            XNonce::from_slice(&wrapped_nonce),
            Payload {
                msg: wrapped,
                aad: V2_WRAP_AAD,
            },
        )
        .map_err(|_| BackupError::AuthenticationFailed)?;
    let dek =
        SecretKey::try_from_slice(&dek_bytes).map_err(|_| BackupError::AuthenticationFailed)?;
    let plaintext = XChaCha20Poly1305::new_from_slice(dek.as_bytes())
        .map_err(|_| BackupError::AuthenticationFailed)?
        .decrypt(
            XNonce::from_slice(&archive_nonce),
            Payload {
                msg: &bytes[offset..],
                aad: &bytes[..offset],
            },
        )
        .map_err(|_| BackupError::AuthenticationFailed)?;
    if plaintext.len() > MAX_ARCHIVE_BYTES {
        return Err(BackupError::InvalidArchive(
            "archive payload exceeds size limit",
        ));
    }
    let payload: ArchivePayload = postcard::from_bytes(&plaintext)
        .map_err(|_| BackupError::InvalidArchive("archive decoding failed"))?;
    validate_payload(&payload, &dek).map_err(|error| match error {
        BackupError::AuthenticationFailed => BackupError::AuthenticationFailed,
        other => other,
    })?;
    Ok((payload, dek))
}

fn take_v2_bytes<'a>(
    bytes: &'a [u8],
    offset: &mut usize,
    len: usize,
) -> Result<&'a [u8], BackupError> {
    let end = offset
        .checked_add(len)
        .ok_or(BackupError::NumericOverflow)?;
    let slice = bytes
        .get(*offset..end)
        .ok_or(BackupError::InvalidArchive("archive header is truncated"))?;
    *offset = end;
    Ok(slice)
}

fn take_v2_array<const N: usize>(bytes: &[u8], offset: &mut usize) -> Result<[u8; N], BackupError> {
    let slice = take_v2_bytes(bytes, offset, N)?;
    slice
        .try_into()
        .map_err(|_| BackupError::InvalidArchive("archive header is truncated"))
}

fn write_owner_only_atomic(path: &Path, bytes: &[u8]) -> Result<(), BackupError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let mut last_collision = None;
    for _ in 0..8 {
        let suffix =
            getrandom::u32().map_err(|_| BackupError::InvalidArchive("randomness unavailable"))?;
        let temporary = path.with_extension(format!("tmp-{suffix:08x}"));
        match write_owner_only_atomic_candidate(path, &temporary, bytes) {
            Err(BackupError::Io(error)) if error.kind() == io::ErrorKind::AlreadyExists => {
                last_collision = Some(error);
            }
            result => return result,
        }
    }
    Err(BackupError::Io(last_collision.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "temporary backup name collision",
        )
    })))
}

fn write_owner_only_atomic_candidate(
    destination: &Path,
    temporary: &Path,
    encrypted: &[u8],
) -> Result<(), BackupError> {
    write_owner_only_atomic_candidate_with(destination, temporary, encrypted, || Ok(()))
}

fn write_owner_only_atomic_candidate_with<F>(
    destination: &Path,
    temporary: &Path,
    encrypted: &[u8],
    before_rename: F,
) -> Result<(), BackupError>
where
    F: FnOnce() -> io::Result<()>,
{
    let mut file = create_owner_only_new(temporary).map_err(BackupError::Io)?;
    if let Err(error) = (|| {
        use io::Write;
        file.write_all(encrypted)?;
        file.sync_all()
    })() {
        drop(file);
        let _ = fs::remove_file(temporary);
        return Err(error.into());
    }
    drop(file);
    if let Err(error) = before_rename() {
        let _ = fs::remove_file(temporary);
        return Err(error.into());
    }
    if let Err(error) = fs::rename(temporary, destination) {
        let _ = fs::remove_file(temporary);
        return Err(error.into());
    }
    Ok(())
}

#[cfg(unix)]
fn create_owner_only_new(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let file = options.open(path)?;
    if let Err(error) = file.set_permissions(fs::Permissions::from_mode(0o600)) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(error);
    }
    Ok(file)
}

#[cfg(not(unix))]
fn create_owner_only_new(path: &Path) -> io::Result<fs::File> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

/// Restore a v2 encrypted archive into a destination vault path.
pub fn restore_from_path(
    archive_path: impl AsRef<Path>,
    destination_vault_path: impl AsRef<Path>,
    backup_password: &[u8],
    new_master_password: &[u8],
) -> Result<RestoreResult, BackupError> {
    if backup_password.is_empty()
        || backup_password.len() > MAX_BACKUP_PASSWORD_BYTES
        || new_master_password.is_empty()
        || new_master_password.len() > MAX_BACKUP_PASSWORD_BYTES
    {
        return Err(BackupError::AuthenticationFailed);
    }
    let archive_path = archive_path.as_ref();
    let destination_vault_path = destination_vault_path.as_ref();
    if paths_equal(archive_path, destination_vault_path) {
        return Err(BackupError::SourceEqualsDestination);
    }
    let metadata = fs::metadata(archive_path)?;
    if metadata.len() > MAX_ARCHIVE_BYTES as u64 {
        return Err(BackupError::InvalidArchive("archive exceeds size limit"));
    }
    let bytes = fs::read(archive_path)?;
    let (payload, dek) = read_v2_archive(&bytes, backup_password)?;
    restore_v2_payload(payload, dek, destination_vault_path, new_master_password)
}

fn restore_v2_payload(
    payload: ArchivePayload,
    dek: SecretKey,
    destination: &Path,
    new_master_password: &[u8],
) -> Result<RestoreResult, BackupError> {
    let existed = destination.exists();
    let mut db = Db::open(destination).map_err(|error| match error {
        crate::storage::DbError::Sql(error) => BackupError::Sql(error),
        crate::storage::DbError::Io(error) => BackupError::Io(error),
        crate::storage::DbError::Migration(_) => {
            BackupError::InvalidArchive("database migration failed")
        }
    })?;
    let fresh_vault = fresh_membership_recovery(&payload);
    let target_vault_id = if fresh_vault {
        VaultId::try_new().map_err(|_| BackupError::InvalidArchive("randomness unavailable"))?
    } else {
        payload.vault_id
    };
    let local_ed25519 = Ed25519Keypair::generate()?;
    let local_x25519 = X25519Keypair::generate()?;
    let local_meta = make_local_meta(target_vault_id, &dek, new_master_password)?;
    let private_material = Zeroizing::new(
        postcard::to_allocvec(&(
            local_ed25519.private_key_bytes(),
            local_x25519.private_key_bytes(),
        ))
        .map_err(|_| BackupError::InvalidArchive("private key encoding failed"))?,
    );
    let encrypted_private_material = seal_private_material(private_material.as_slice(), &dek)?;
    let result = db.transaction(|tx| -> Result<RestoreResult, BackupError> {
        clear_database(tx)?;
        if fresh_vault {
            restore_fresh_vault(
                tx,
                &payload,
                target_vault_id,
                &dek,
                &local_ed25519,
                &local_x25519,
                &encrypted_private_material,
                Some(&local_meta),
            )?;
        } else {
            restore_existing_vault(
                tx,
                &payload,
                &dek,
                &local_ed25519,
                &local_x25519,
                &encrypted_private_material,
                Some(&local_meta),
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
    });
    match result {
        Ok(result) => Ok(result),
        Err(error) => {
            drop(db);
            if !existed {
                remove_database_files(destination);
            }
            Err(error)
        }
    }
}

fn make_local_meta(
    vault_id: VaultId,
    dek: &SecretKey,
    master_password: &[u8],
) -> Result<VaultMetaArchive, BackupError> {
    let salt =
        kdf::random_salt().map_err(|_| BackupError::InvalidArchive("randomness unavailable"))?;
    let params = kdf::Argon2Params::v1();
    let kek = kdf::derive_kek(master_password, salt, params)?;
    let wrapped = keys::wrap_dek(&kek, dek)?;
    Ok(VaultMetaArchive {
        format_version: 1,
        vault_id,
        kdf_algorithm: "argon2id".into(),
        kdf_salt: salt.to_vec(),
        argon2_memory_kib: i64::from(params.memory_kib),
        argon2_iterations: i64::from(params.iterations),
        argon2_parallelism: i64::from(params.parallelism),
        key_wrap_algorithm: "xchacha20poly1305".into(),
        wrapped_dek_nonce: wrapped.nonce.as_bytes().to_vec(),
        wrapped_dek: wrapped.ciphertext,
    })
}

fn remove_database_files(path: &Path) {
    let _ = fs::remove_file(path);
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        let _ = fs::remove_file(PathBuf::from(sidecar));
    }
}

/// Restore an old raw-key archive into an already-open database.
pub fn restore_from_db_path(
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
    validate_membership_archive(payload)?;
    let mut change_ids = std::collections::HashSet::new();
    let mut origin_sequences = std::collections::HashSet::new();
    for archived in &payload.changes {
        let change = &archived.change;
        if change.vault_id != payload.vault_id
            || change.signature.len() != 64
            || change.ciphertext.len() < 16
            || change.ciphertext.len() > journal::MAX_CHANGE_CIPHERTEXT_BYTES
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

fn validate_membership_archive(payload: &ArchivePayload) -> Result<(), BackupError> {
    if payload.memberships.is_empty() {
        return Ok(());
    }
    if payload.memberships.len() > membership::MAX_MEMBERSHIP_RECORDS {
        return Err(BackupError::InvalidArchive(
            "membership record limit exceeded",
        ));
    }
    let mut records = Vec::with_capacity(payload.memberships.len());
    let mut hashes = std::collections::HashSet::new();
    for membership in &payload.memberships {
        if membership.vault_id != payload.vault_id
            || membership.record_hash.len() != 32
            || membership.record.len() > MAX_RECORD_BYTES
            || !hashes.insert(membership.record_hash.clone())
        {
            return Err(BackupError::InvalidArchive("invalid membership record"));
        }
        match MembershipRecord::from_canonical_bytes(&membership.record) {
            Ok(record) => {
                if record.vault_id() != payload.vault_id
                    || membership.record_type != membership_type(&record)
                    || record
                        .record_hash()
                        .map_err(|_| BackupError::InvalidArchive("invalid membership record"))?
                        .as_bytes()
                        != membership.record_hash.as_slice()
                {
                    return Err(BackupError::InvalidArchive("invalid membership record"));
                }
                records.push(record);
            }
            Err(_) if is_legacy_genesis(membership, payload.vault_id) => continue,
            Err(_) => return Err(BackupError::InvalidArchive("invalid membership record")),
        }
    }
    if records.is_empty() {
        if payload.memberships.len() == 1
            && is_legacy_genesis(&payload.memberships[0], payload.vault_id)
        {
            return Ok(());
        }
        return Err(BackupError::InvalidArchive("invalid membership record"));
    }
    if records.len() != payload.memberships.len() {
        return Err(BackupError::InvalidArchive("mixed membership formats"));
    }
    membership::validate_membership(payload.vault_id, &records)
        .map_err(|_| BackupError::InvalidArchive("invalid membership chain"))?;
    Ok(())
}

fn fresh_membership_recovery(payload: &ArchivePayload) -> bool {
    payload.memberships.is_empty()
        || (payload.memberships.len() == 1
            && is_legacy_genesis(&payload.memberships[0], payload.vault_id))
}

fn membership_type(record: &MembershipRecord) -> &'static str {
    match record {
        MembershipRecord::Genesis(_) => "genesis",
        MembershipRecord::Admission(_) => "admission",
        MembershipRecord::Acceptance(_) => "acceptance",
    }
}

fn is_legacy_genesis(membership: &MembershipArchive, vault_id: VaultId) -> bool {
    if membership.record_type != "genesis" || membership.vault_id != vault_id {
        return false;
    }
    let Ok(legacy) = postcard::from_bytes::<GenesisRecord>(&membership.record) else {
        return false;
    };
    legacy.vault_id == vault_id
        && legacy.device_id == DeviceId::from_public_key(legacy.ed25519_public_key)
        && Sha256::digest(&membership.record).as_slice() == membership.record_hash.as_slice()
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
    meta: Option<&VaultMetaArchive>,
) -> Result<(), BackupError> {
    if let Some(meta) = meta {
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

#[allow(clippy::too_many_arguments)]
fn restore_fresh_vault(
    tx: &Transaction<'_>,
    payload: &ArchivePayload,
    vault_id: VaultId,
    key: &SecretKey,
    local_ed25519: &Ed25519Keypair,
    local_x25519: &X25519Keypair,
    encrypted_private_material: &[u8],
    meta: Option<&VaultMetaArchive>,
) -> Result<(), BackupError> {
    if let Some(meta) = meta {
        let mut fresh_meta = meta.clone();
        fresh_meta.vault_id = vault_id;
        insert_vault_meta(tx, &fresh_meta)?;
    }
    let device_id = DeviceId::from_public_key(local_ed25519.public_key_bytes());
    let creator = membership::DeviceIdentity::new_signed(
        "This device",
        membership::MEMBERSHIP_FORMAT_VERSION,
        local_ed25519,
        local_x25519.public_key_bytes(),
    )
    .map_err(|_| BackupError::InvalidArchive("genesis encoding failed"))?;
    let genesis =
        membership::create_genesis(vault_id, creator, crate::Hlc::new(0, 0), local_ed25519)
            .map_err(|_| BackupError::InvalidArchive("genesis encoding failed"))?;
    membership::insert_membership_record_tx(tx, &genesis)
        .map_err(|_| BackupError::InvalidArchive("genesis encoding failed"))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_width_archive_reads_reject_truncation_without_advancing() {
        let mut offset = 0;
        assert!(matches!(
            take_v2_array::<4>(&[1, 2, 3], &mut offset),
            Err(BackupError::InvalidArchive(_))
        ));
        assert_eq!(offset, 0);
    }

    #[test]
    fn v1_archive_header_truncation_is_rejected_at_every_boundary() {
        let key = SecretKey::from_bytes([7; 32]);
        for length in 0..(8 + 1 + 24 + 16) {
            assert!(
                open_archive(&vec![0; length], &key).is_err(),
                "length {length}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn backup_failure_preserves_destination_and_cleans_temporary() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let root =
            std::env::temp_dir().join(format!("locker-backup-atomic-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir(&root).unwrap();
        let destination = root.join("destination.lockbak");
        let temporary = root.join("candidate.tmp");
        fs::write(&destination, b"old archive").unwrap();
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o666)).unwrap();
        let result = write_owner_only_atomic_candidate_with(
            &destination,
            &temporary,
            b"NOXBACK2encrypted",
            || {
                assert_eq!(fs::read(&temporary).unwrap(), b"NOXBACK2encrypted");
                let metadata = fs::symlink_metadata(&temporary).unwrap();
                assert!(metadata.file_type().is_file());
                assert_eq!(metadata.uid(), fs::symlink_metadata(&root).unwrap().uid());
                assert_eq!(metadata.permissions().mode() & 0o7777, 0o600);
                Err(io::Error::new(io::ErrorKind::PermissionDenied, "injected"))
            },
        );
        assert!(
            matches!(result, Err(BackupError::Io(error)) if error.kind() == io::ErrorKind::PermissionDenied)
        );
        assert_eq!(fs::read(&destination).unwrap(), b"old archive");
        assert_eq!(
            fs::symlink_metadata(&destination)
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            0o666
        );
        assert!(!temporary.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn temporary_symlink_collision_is_not_followed() {
        let root =
            std::env::temp_dir().join(format!("locker-backup-symlink-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir(&root).unwrap();
        let target = root.join("target");
        let temporary = root.join("candidate.tmp");
        let destination = root.join("destination.lockbak");
        fs::write(&target, b"target").unwrap();
        std::os::unix::fs::symlink(&target, &temporary).unwrap();
        let result = write_owner_only_atomic_candidate(&destination, &temporary, b"archive");
        assert!(
            matches!(result, Err(BackupError::Io(error)) if error.kind() == io::ErrorKind::AlreadyExists)
        );
        assert_eq!(fs::read(&target).unwrap(), b"target");
        assert!(
            fs::symlink_metadata(&temporary)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let _ = fs::remove_dir_all(root);
    }
}
