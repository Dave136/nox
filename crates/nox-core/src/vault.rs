//! Vault creation, unlocking, and secret lifetime management.

use crate::{
    Hlc,
    crypto::{
        CipherError, cipher,
        kdf::{self, Argon2Params, KdfError},
        keys::{self, Ed25519Keypair, KeyError, WrappedDek, X25519Keypair},
        secret::Dek,
    },
    ids::{ChangeId, DeviceId, ItemId, VaultId},
    item::{ITEM_SCHEMA_VERSION, ItemPayload, ItemPayloadError},
    journal::{self, Change, JournalError},
    membership::{self, AuthorizationSnapshot, MembershipError, ValidatedMembership},
    merge,
    pairing::{PairingStore, PairingVaultPackage, PreparedJoiningDevice},
    storage::{Db, DbError},
};
use rusqlite::{OptionalExtension, params, types::Type};
use std::{
    collections::BTreeMap,
    fmt, io,
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

const FORMAT_VERSION: i64 = 1;
const KDF_ALGORITHM: &str = "argon2id";
const KEY_WRAP_ALGORITHM: &str = "xchacha20poly1305";
const AEAD_TAG_LENGTH: usize = 16;
const LIVE_ITEMS_QUERY: &str = "SELECT changes.change_id, changes.vault_id, changes.item_id,
    changes.parent_change_ids, changes.origin_device_id, changes.origin_seq,
    changes.hlc_physical_ms, changes.hlc_logical, changes.operation,
    changes.payload_schema_version, changes.nonce, changes.ciphertext,
    changes.signature
    FROM items JOIN changes ON changes.change_id = items.winning_change_id
    WHERE items.deleted = 0";
const GET_ITEM_QUERY: &str = "SELECT changes.change_id, changes.vault_id, changes.item_id,
    changes.parent_change_ids, changes.origin_device_id, changes.origin_seq,
    changes.hlc_physical_ms, changes.hlc_logical, changes.operation,
    changes.payload_schema_version, changes.nonce, changes.ciphertext,
    changes.signature
    FROM items JOIN changes ON changes.change_id = items.winning_change_id
    WHERE items.item_id = ?1 AND items.deleted = 0";
const LAST_UPSERT_QUERY: &str = "SELECT changes.change_id, changes.vault_id, changes.item_id,
    changes.parent_change_ids, changes.origin_device_id, changes.origin_seq,
    changes.hlc_physical_ms, changes.hlc_logical, changes.operation,
    changes.payload_schema_version, changes.nonce, changes.ciphertext,
    changes.signature
    FROM changes
    WHERE changes.item_id = ?1 AND changes.operation = 0
    ORDER BY changes.hlc_physical_ms DESC, changes.hlc_logical DESC,
             changes.origin_device_id DESC, changes.origin_seq DESC
    LIMIT 1";

/// Errors returned by vault lifecycle operations.
#[derive(Debug)]
pub enum VaultError {
    /// A filesystem operation failed.
    Io(io::Error),
    /// SQLite opening or mutation failed.
    Db(DbError),
    /// Password KDF failed.
    Kdf(KdfError),
    /// Key generation, wrapping, or private material handling failed.
    Key(KeyError),
    /// OS randomness failed while creating vault material.
    Randomness(getrandom::Error),
    /// Password or authenticated vault material was invalid.
    IncorrectPasswordOrCorruptVault,
    /// A vault already exists at the requested path.
    VaultAlreadyExists,
    /// No vault exists at the requested path.
    VaultNotFound,
    /// The platform home/data directory could not be resolved.
    NoHomeDirectory,
    /// A journal mutation failed.
    Journal(JournalError),
    /// An item ciphertext failed authentication or decryption.
    Cipher(CipherError),
    /// An item payload could not be encoded or decoded.
    ItemPayload(ItemPayloadError),
    /// The encrypted envelope and JSON payload declare different schemas.
    SchemaVersionMismatch { envelope: u32, payload: u16 },
    /// The requested item has no prior revision.
    ItemNotFound,
    /// Membership validation or local authorization failed.
    Membership(MembershipError),
}

impl fmt::Display for VaultError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "vault I/O error: {error}"),
            Self::Db(error) => write!(formatter, "vault database error: {error}"),
            Self::Kdf(error) => write!(formatter, "vault KDF error: {error}"),
            Self::Key(error) => write!(formatter, "vault key error: {error}"),
            Self::Randomness(error) => write!(formatter, "vault randomness error: {error}"),
            Self::IncorrectPasswordOrCorruptVault => {
                formatter.write_str("incorrect password or corrupted vault")
            }
            Self::VaultAlreadyExists => formatter.write_str("vault already exists"),
            Self::VaultNotFound => formatter.write_str("vault not found"),
            Self::NoHomeDirectory => formatter.write_str("home directory is unavailable"),
            Self::Journal(error) => write!(formatter, "vault journal error: {error}"),
            Self::Cipher(error) => write!(formatter, "vault cipher error: {error}"),
            Self::ItemPayload(error) => write!(formatter, "vault item payload error: {error}"),
            Self::SchemaVersionMismatch { envelope, payload } => write!(
                formatter,
                "item schema version mismatch: envelope {envelope}, payload {payload}"
            ),
            Self::ItemNotFound => formatter.write_str("item not found"),
            Self::Membership(error) => write!(formatter, "vault membership error: {error}"),
        }
    }
}

impl std::error::Error for VaultError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Db(error) => Some(error),
            Self::Kdf(error) => Some(error),
            Self::Key(error) => Some(error),
            Self::Randomness(error) => Some(error),
            Self::Journal(error) => Some(error),
            Self::Cipher(error) => Some(error),
            Self::ItemPayload(error) => Some(error),
            Self::IncorrectPasswordOrCorruptVault
            | Self::VaultAlreadyExists
            | Self::VaultNotFound
            | Self::NoHomeDirectory
            | Self::SchemaVersionMismatch { .. }
            | Self::ItemNotFound => None,
            Self::Membership(error) => Some(error),
        }
    }
}

impl From<io::Error> for VaultError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<DbError> for VaultError {
    fn from(error: DbError) -> Self {
        Self::Db(error)
    }
}

impl From<rusqlite::Error> for VaultError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Db(DbError::from(error))
    }
}

impl From<KdfError> for VaultError {
    fn from(error: KdfError) -> Self {
        Self::Kdf(error)
    }
}

impl From<KeyError> for VaultError {
    fn from(error: KeyError) -> Self {
        Self::Key(error)
    }
}

impl From<getrandom::Error> for VaultError {
    fn from(error: getrandom::Error) -> Self {
        Self::Randomness(error)
    }
}

impl From<JournalError> for VaultError {
    fn from(error: JournalError) -> Self {
        Self::Journal(error)
    }
}

impl From<CipherError> for VaultError {
    fn from(error: CipherError) -> Self {
        Self::Cipher(error)
    }
}

impl From<ItemPayloadError> for VaultError {
    fn from(error: ItemPayloadError) -> Self {
        Self::ItemPayload(error)
    }
}

impl From<MembershipError> for VaultError {
    fn from(error: MembershipError) -> Self {
        Self::Membership(error)
    }
}

/// An unlocked vault and the secrets required to operate on it.
pub struct Vault {
    db: Db,
    vault_id: VaultId,
    device_id: DeviceId,
    dek: Dek,
    ed25519: Ed25519Keypair,
    #[allow(dead_code)]
    x25519: X25519Keypair,
}

/// Narrow, unlocked capability for the headless sync service.
///
/// The capability owns typed secret holders but intentionally exposes neither
/// their bytes nor the underlying SQLite connection.  A service must be
/// stopped before the originating vault is locked.
pub struct UnlockedSyncAccess {
    path: PathBuf,
    vault_id: VaultId,
    device_id: DeviceId,
    identity: membership::DeviceIdentity,
    ed25519: Ed25519Keypair,
    x25519: X25519Keypair,
    dek: Dek,
}

impl fmt::Debug for UnlockedSyncAccess {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("UnlockedSyncAccess(<redacted>)")
    }
}

impl UnlockedSyncAccess {
    /// Duplicate only the bounded typed owners needed by one blocking core job.
    /// The duplicate retains redacted diagnostics and zeroizes on drop.
    #[must_use]
    pub fn clone_for_blocking_job(&self) -> Self {
        Self {
            path: self.path.clone(),
            vault_id: self.vault_id,
            device_id: self.device_id,
            identity: self.identity.clone(),
            ed25519: Ed25519Keypair::from_private_bytes(self.ed25519.private_key_bytes()),
            x25519: X25519Keypair::from_private_bytes(self.x25519.private_key_bytes()),
            dek: self.dek.clone(),
        }
    }

    #[must_use]
    pub fn vault_id(&self) -> VaultId {
        self.vault_id
    }

    #[must_use]
    pub fn local_device_id(&self) -> DeviceId {
        self.device_id
    }

    #[must_use]
    pub fn local_identity(&self) -> &membership::DeviceIdentity {
        &self.identity
    }

    pub fn authorized_peer_keys(&self) -> Result<BTreeMap<DeviceId, [u8; 32]>, VaultError> {
        let authorization = self.authorization_snapshot()?;
        let mut peers = BTreeMap::new();
        for identity in authorization.membership.active_members() {
            if identity.device_id == self.device_id
                || authorization.is_locally_blocked(identity.device_id)
            {
                continue;
            }
            if let Some(keys) = authorization.membership.member_keys(identity.device_id) {
                peers.insert(identity.device_id, keys.x25519);
            }
        }
        Ok(peers)
    }

    pub fn authorization_snapshot(&self) -> Result<AuthorizationSnapshot, VaultError> {
        let db = Db::open(&self.path)?;
        Ok(membership::load_authorization(&db, self.vault_id)?)
    }

    pub fn open_pairing_store(&self) -> Result<PairingStore, VaultError> {
        let db = Db::open(&self.path)?;
        Ok(PairingStore::new(
            db,
            self.vault_id,
            self.device_id,
            self.dek.clone(),
            Ed25519Keypair::from_private_bytes(self.ed25519.private_key_bytes()),
        ))
    }

    pub fn open_replication_store(&self) -> Result<journal::ReplicationStore, VaultError> {
        let db = Db::open(&self.path)?;
        Ok(journal::ReplicationStore::new(db, self.vault_id))
    }

    pub fn block_device(&self, device_id: DeviceId) -> Result<(), VaultError> {
        let mut db = Db::open(&self.path)?;
        membership::block_device(
            &mut db,
            self.device_id,
            device_id,
            Hlc::new(now_millis(), 0),
        )?;
        Ok(())
    }

    pub fn unblock_device(&self, device_id: DeviceId) -> Result<(), VaultError> {
        let mut db = Db::open(&self.path)?;
        membership::unblock_device(&mut db, device_id)?;
        Ok(())
    }

    /// Return a typed copy for one Noise handshake.  The key type redacts
    /// diagnostics and zeroizes its private material on drop.
    #[must_use]
    pub fn local_noise_keypair(&self) -> X25519Keypair {
        X25519Keypair::from_private_bytes(self.x25519.private_key_bytes())
    }
}

impl fmt::Debug for Vault {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Vault(<redacted>)")
    }
}

impl Vault {
    /// Prepare a v2 backup export without scanning or serializing on the caller thread.
    pub fn prepare_backup_export(
        &self,
        backup_password: &[u8],
        destination: impl AsRef<Path>,
    ) -> Result<crate::backup::BackupExportRequest, crate::backup::BackupError> {
        let source = self
            .db
            .path()
            .ok_or(crate::backup::BackupError::UnsupportedSource)?;
        crate::backup::BackupExportRequest::new(source, backup_password, destination, &self.dek)
    }

    /// Create and persist a new vault at `path`.
    pub fn create(password: &[u8], path: impl AsRef<Path>) -> Result<Self, VaultError> {
        let mut db = Db::open(path)?;
        let (vault_id, device_id, dek, ed25519, x25519) = db.transaction(|tx| {
            if tx
                .query_row("SELECT singleton FROM vault_meta LIMIT 1", [], |_| Ok(()))
                .optional()?
                .is_some()
            {
                return Err(VaultError::VaultAlreadyExists);
            }

            let vault_id = VaultId::try_new()?;
            let salt = kdf::random_salt()?;
            let params = Argon2Params::v1();
            let kek = kdf::derive_kek(password, salt, params)?;
            let dek = Dek::random()?;
            let wrapped = keys::wrap_dek(&kek, &dek)?;

            tx.execute(
                "INSERT INTO vault_meta (
                    singleton, format_version, vault_id, kdf_algorithm, kdf_salt,
                    argon2_memory_kib, argon2_iterations, argon2_parallelism,
                    key_wrap_algorithm, wrapped_dek_nonce, wrapped_dek
                ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    FORMAT_VERSION,
                    vault_id.as_ref(),
                    KDF_ALGORITHM,
                    salt.as_slice(),
                    i64::from(params.memory_kib),
                    i64::from(params.iterations),
                    i64::from(params.parallelism),
                    KEY_WRAP_ALGORITHM,
                    wrapped.nonce.as_bytes(),
                    wrapped.ciphertext,
                ],
            )?;

            let ed25519 = Ed25519Keypair::generate()?;
            let x25519 = X25519Keypair::generate()?;
            let private_material = Zeroizing::new(
                postcard::to_allocvec(&(ed25519.private_key_bytes(), x25519.private_key_bytes()))
                    .map_err(|_| KeyError::Invalid)?,
            );
            let encrypted_private_material =
                keys::seal_fixed_aad(&dek, keys::PRIVATE_KEY_AAD, private_material.as_slice())?;
            let device_id = DeviceId::from_public_key(ed25519.public_key_bytes());
            tx.execute(
                "INSERT INTO local_device (
                    singleton, device_id, ed25519_public_key, x25519_public_key,
                    encrypted_private_key_material
                ) VALUES (1, ?1, ?2, ?3, ?4)",
                params![
                    device_id.as_ref(),
                    ed25519.public_key_bytes().as_slice(),
                    x25519.public_key_bytes().as_slice(),
                    encrypted_private_material,
                ],
            )?;

            let creator = membership::DeviceIdentity::new_signed(
                "This device",
                membership::MEMBERSHIP_FORMAT_VERSION,
                &ed25519,
                x25519.public_key_bytes(),
            )?;
            let genesis =
                membership::create_genesis(vault_id, creator, Hlc::new(now_millis(), 0), &ed25519)?;
            membership::insert_membership_record_tx(tx, &genesis)?;

            Ok((vault_id, device_id, dek, ed25519, x25519))
        })?;

        Ok(Self {
            db,
            vault_id,
            device_id,
            dek,
            ed25519,
            x25519,
        })
    }

    /// Return the validated immutable membership snapshot for this vault.
    pub fn membership(&self) -> Result<ValidatedMembership, VaultError> {
        Ok(membership::load_membership(&self.db, self.vault_id)?)
    }

    /// Return membership plus installation-local blocking policy.
    pub fn authorization(&self) -> Result<AuthorizationSnapshot, VaultError> {
        Ok(membership::load_authorization(&self.db, self.vault_id)?)
    }

    /// Create the narrow capability consumed by the headless sync service.
    pub fn open_sync_access(&self) -> Result<UnlockedSyncAccess, VaultError> {
        let path = self
            .db
            .path()
            .ok_or_else(|| {
                VaultError::Db(DbError::Io(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "sync access requires a file-backed vault",
                )))
            })?
            .to_owned();
        let authorization = self.authorization()?;
        if !authorization.is_current_member(self.device_id) {
            return Err(VaultError::Membership(MembershipError::InvalidIdentity(
                "local device is not active",
            )));
        }
        let identity = authorization
            .membership
            .identity(self.device_id)
            .cloned()
            .ok_or(VaultError::Membership(MembershipError::UnknownDevice))?;
        let access = UnlockedSyncAccess {
            path,
            vault_id: self.vault_id,
            device_id: self.device_id,
            identity,
            ed25519: Ed25519Keypair::from_private_bytes(self.ed25519.private_key_bytes()),
            x25519: X25519Keypair::from_private_bytes(self.x25519.private_key_bytes()),
            dek: self.dek.clone(),
        };
        // Validate independent store access before returning a capability.
        let _ = access.open_pairing_store()?;
        let _ = access.open_replication_store()?;
        Ok(access)
    }

    /// Open an independent synchronous store for pairing jobs.
    pub fn open_pairing_store(&self) -> Result<PairingStore, VaultError> {
        let path = self.db.path().ok_or_else(|| {
            VaultError::Db(DbError::Io(io::Error::new(
                io::ErrorKind::Unsupported,
                "pairing stores require a file-backed vault",
            )))
        })?;
        let db = Db::open(path)?;
        Ok(PairingStore::new(
            db,
            self.vault_id,
            self.device_id,
            self.dek.clone(),
            Ed25519Keypair::from_private_bytes(self.ed25519.private_key_bytes()),
        ))
    }

    /// Open an independent key-free store for authenticated replication.
    pub fn open_replication_store(&self) -> Result<journal::ReplicationStore, VaultError> {
        let path = self.db.path().ok_or_else(|| {
            VaultError::Db(DbError::Io(io::Error::new(
                io::ErrorKind::Unsupported,
                "replication stores require a file-backed vault",
            )))
        })?;
        let db = Db::open(path)?;
        let stored_vault = db
            .connection()
            .query_row("SELECT vault_id FROM vault_meta LIMIT 1", [], |row| {
                row.get::<_, Vec<u8>>(0)
            })
            .optional()?;
        let Some(stored_vault) = stored_vault else {
            return Err(VaultError::IncorrectPasswordOrCorruptVault);
        };
        let stored_vault: [u8; 16] = stored_vault
            .try_into()
            .map_err(|_| VaultError::IncorrectPasswordOrCorruptVault)?;
        let stored_vault = VaultId::from_bytes(stored_vault);
        if stored_vault != self.vault_id {
            return Err(VaultError::IncorrectPasswordOrCorruptVault);
        }
        Ok(journal::ReplicationStore::new(db, self.vault_id))
    }

    /// Generate the signing and Noise identities for a fresh joining profile.
    pub fn prepare_joining_device(
        display_name: &str,
        protocol_version: u16,
    ) -> Result<PreparedJoiningDevice, VaultError> {
        let ed25519 = Ed25519Keypair::generate()?;
        let x25519 = X25519Keypair::generate()?;
        let identity = membership::DeviceIdentity::new_signed(
            display_name,
            protocol_version,
            &ed25519,
            x25519.public_key(),
        )?;
        Ok(PreparedJoiningDevice::new(ed25519, x25519, identity))
    }

    /// Create a new profile from an authenticated inviter package atomically.
    pub fn create_from_pairing(
        destination: impl AsRef<Path>,
        master_password: &[u8],
        prepared: PreparedJoiningDevice,
        package: PairingVaultPackage,
        accepted_at: Hlc,
    ) -> Result<(Self, membership::MembershipAcceptance), VaultError> {
        let path = destination.as_ref().to_path_buf();
        if path.exists() {
            return Err(VaultError::VaultAlreadyExists);
        }
        prepared.identity.verify()?;
        let validated = membership::validate_membership(package.vault_id, &package.records)?;
        let admission = match validated.record(package.admission_hash) {
            Some(membership::MembershipRecord::Admission(value)) => value,
            _ => {
                return Err(VaultError::Membership(MembershipError::InvalidRecord(
                    "pairing admission",
                )));
            }
        };
        if admission.admitted_device != prepared.identity
            || admission.invited_by != package.inviter_device_id
            || validated.is_active(prepared.device_id())
            || !validated.is_active(package.inviter_device_id)
        {
            return Err(VaultError::Membership(MembershipError::InvalidIdentity(
                "pairing package identity",
            )));
        }
        let physical_ms = i64::try_from(accepted_at.physical_ms).map_err(|_| {
            VaultError::Membership(MembershipError::InvalidRecord("acceptance HLC"))
        })?;

        let mut db = Db::open(&path)?;
        let result = db.transaction(|tx| {
            if tx
                .query_row("SELECT singleton FROM vault_meta LIMIT 1", [], |_| Ok(()))
                .optional()?
                .is_some()
            {
                return Err(VaultError::VaultAlreadyExists);
            }

            let salt = kdf::random_salt()?;
            let params = Argon2Params::v1();
            let kek = kdf::derive_kek(master_password, salt, params)?;
            let wrapped = keys::wrap_dek(&kek, &package.dek)?;
            tx.execute(
                "INSERT INTO vault_meta (
                    singleton, format_version, vault_id, kdf_algorithm, kdf_salt,
                    argon2_memory_kib, argon2_iterations, argon2_parallelism,
                    key_wrap_algorithm, wrapped_dek_nonce, wrapped_dek
                ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    FORMAT_VERSION,
                    package.vault_id.as_ref(),
                    KDF_ALGORITHM,
                    salt.as_slice(),
                    i64::from(params.memory_kib),
                    i64::from(params.iterations),
                    i64::from(params.parallelism),
                    KEY_WRAP_ALGORITHM,
                    wrapped.nonce.as_bytes(),
                    wrapped.ciphertext,
                ],
            )?;

            let private_material = Zeroizing::new(
                postcard::to_allocvec(&(
                    prepared.ed25519.private_key_bytes(),
                    prepared.x25519.private_key_bytes(),
                ))
                .map_err(|_| KeyError::Invalid)?,
            );
            let encrypted_private_material = keys::seal_fixed_aad(
                &package.dek,
                keys::PRIVATE_KEY_AAD,
                private_material.as_slice(),
            )?;
            tx.execute(
                "INSERT INTO local_device (
                    singleton, device_id, ed25519_public_key, x25519_public_key,
                    encrypted_private_key_material
                ) VALUES (1, ?1, ?2, ?3, ?4)",
                params![
                    prepared.device_id().as_ref(),
                    prepared.ed25519.public_key_bytes().as_slice(),
                    prepared.x25519.public_key().as_slice(),
                    encrypted_private_material,
                ],
            )?;

            for record in &package.records {
                membership::insert_membership_record_tx(tx, record)?;
            }
            let acceptance_record = membership::create_acceptance(
                package.vault_id,
                package.admission_hash,
                prepared.device_id(),
                accepted_at,
                &prepared.ed25519,
            )?;
            let acceptance = match acceptance_record {
                membership::MembershipRecord::Acceptance(value) => value,
                _ => unreachable!("acceptance constructor returned the wrong record"),
            };
            membership::insert_membership_record_tx(
                tx,
                &membership::MembershipRecord::Acceptance(acceptance.clone()),
            )?;
            tx.execute(
                "INSERT INTO clock_state (vault_id, hlc_physical_ms, hlc_logical, next_origin_seq) VALUES (?1, ?2, ?3, 0)",
                params![
                    package.vault_id.as_ref(),
                    physical_ms,
                    i64::from(accepted_at.logical),
                ],
            )?;
            Ok(acceptance)
        });
        let acceptance = match result {
            Ok(acceptance) => acceptance,
            Err(error) => {
                drop(db);
                remove_new_database_files(&path);
                return Err(error);
            }
        };

        let vault_id = package.vault_id;
        let device_id = prepared.device_id();
        let dek = package.dek;
        let ed25519 = prepared.ed25519;
        let x25519 = prepared.x25519;
        Ok((
            Self {
                db,
                vault_id,
                device_id,
                dek,
                ed25519,
                x25519,
            },
            acceptance,
        ))
    }

    /// Block a known member on this installation.
    pub fn block_member(&mut self, target: DeviceId, at: Hlc) -> Result<(), VaultError> {
        Ok(membership::block_device(
            &mut self.db,
            self.device_id,
            target,
            at,
        )?)
    }

    /// Remove an installation-local member block.
    pub fn unblock_member(&mut self, target: DeviceId) -> Result<bool, VaultError> {
        Ok(membership::unblock_device(&mut self.db, target)?)
    }

    /// Unlock an existing vault without creating missing paths.
    pub fn unlock(password: &[u8], path: impl AsRef<Path>) -> Result<Self, VaultError> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(VaultError::VaultNotFound);
        }
        let db = Db::open(path)?;

        let meta = db
            .connection()
            .query_row(
                "SELECT format_version, vault_id, kdf_algorithm, kdf_salt,
                        argon2_memory_kib, argon2_iterations, argon2_parallelism,
                        key_wrap_algorithm, wrapped_dek_nonce, wrapped_dek
                 FROM vault_meta LIMIT 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Vec<u8>>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, Vec<u8>>(8)?,
                        row.get::<_, Vec<u8>>(9)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            format_version,
            vault_id,
            kdf_algorithm,
            kdf_salt,
            memory_kib,
            iterations,
            parallelism,
            key_wrap_algorithm,
            wrapped_nonce,
            wrapped_ciphertext,
        )) = meta
        else {
            return Err(VaultError::VaultNotFound);
        };

        let corrupt = || VaultError::IncorrectPasswordOrCorruptVault;
        if format_version != FORMAT_VERSION
            || kdf_algorithm != KDF_ALGORITHM
            || key_wrap_algorithm != KEY_WRAP_ALGORITHM
            || kdf_salt.len() != kdf::SALT_LENGTH
            || wrapped_nonce.len() != cipher::NONCE_LENGTH
            || wrapped_ciphertext.len() < AEAD_TAG_LENGTH
        {
            return Err(corrupt());
        }
        let params = match (
            u32::try_from(memory_kib),
            u32::try_from(iterations),
            u32::try_from(parallelism),
        ) {
            (Ok(memory_kib), Ok(iterations), Ok(parallelism)) => {
                Argon2Params::new(memory_kib, iterations, parallelism)
            }
            _ => return Err(corrupt()),
        };
        if params != Argon2Params::v1() {
            return Err(corrupt());
        }
        let vault_id = match vault_id.as_slice().try_into() {
            Ok(bytes) => VaultId::from_bytes(bytes),
            Err(_) => return Err(corrupt()),
        };
        let nonce = match wrapped_nonce.as_slice().try_into() {
            Ok(bytes) => cipher::Nonce::from_bytes(bytes),
            Err(_) => return Err(corrupt()),
        };
        let kek = kdf::derive_kek(password, &kdf_salt, params).map_err(|_| corrupt())?;
        let dek = keys::unwrap_dek(
            &kek,
            &WrappedDek {
                nonce,
                ciphertext: wrapped_ciphertext,
            },
        )
        .map_err(|_| corrupt())?;

        let local_device = db
            .connection()
            .query_row(
                "SELECT device_id, ed25519_public_key, x25519_public_key,
                        encrypted_private_key_material
                 FROM local_device WHERE singleton = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                        row.get::<_, Vec<u8>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((stored_device_id, stored_ed25519, stored_x25519, encrypted_private)) =
            local_device
        else {
            return Err(corrupt());
        };

        let private_material =
            keys::open_fixed_aad(&dek, keys::PRIVATE_KEY_AAD, &encrypted_private)
                .map_err(|_| corrupt())?;
        let (ed25519_private, x25519_private): ([u8; 32], [u8; 32]) =
            postcard::from_bytes(private_material.as_bytes()).map_err(|_| corrupt())?;
        let ed25519 = Ed25519Keypair::from_private_bytes(ed25519_private);
        let x25519 = X25519Keypair::from_private_bytes(x25519_private);
        let stored_device_id: [u8; 32] = stored_device_id
            .as_slice()
            .try_into()
            .map_err(|_| corrupt())?;
        let stored_ed25519: [u8; 32] = stored_ed25519
            .as_slice()
            .try_into()
            .map_err(|_| corrupt())?;
        let stored_x25519: [u8; 32] = stored_x25519.as_slice().try_into().map_err(|_| corrupt())?;
        let device_id = DeviceId::from_public_key(ed25519.public_key_bytes());
        if device_id != DeviceId::from_bytes(stored_device_id)
            || ed25519.public_key_bytes() != stored_ed25519
            || x25519.public_key_bytes() != stored_x25519
        {
            return Err(corrupt());
        }

        Ok(Self {
            db,
            vault_id,
            device_id,
            dek,
            ed25519,
            x25519,
        })
    }

    /// Consume the unlocked handle and drop its database and secrets.
    pub fn lock(self) {
        drop(self);
    }

    /// Create a new encrypted item revision and return its generated id.
    pub fn create_item(&mut self, payload: &ItemPayload) -> Result<ItemId, VaultError> {
        let encoded = payload.to_json_bytes()?;
        let item_id = ItemId::new();
        journal::create_local_change(
            &mut self.db,
            self.vault_id,
            item_id,
            &self.ed25519,
            &self.dek,
            cipher::Operation::Upsert,
            encoded,
            u32::from(ITEM_SCHEMA_VERSION),
            now_millis(),
        )?;
        Ok(item_id)
    }

    /// Append an encrypted update to an existing item, including tombstone restoration.
    pub fn update_item(
        &mut self,
        item_id: ItemId,
        payload: &ItemPayload,
    ) -> Result<(), VaultError> {
        let encoded = payload.to_json_bytes()?;
        if !self.item_exists(item_id)? {
            return Err(VaultError::ItemNotFound);
        }
        journal::create_local_change(
            &mut self.db,
            self.vault_id,
            item_id,
            &self.ed25519,
            &self.dek,
            cipher::Operation::Upsert,
            encoded,
            u32::from(ITEM_SCHEMA_VERSION),
            now_millis(),
        )?;
        Ok(())
    }

    /// Append an encrypted tombstone to an existing item.
    pub fn delete_item(&mut self, item_id: ItemId) -> Result<(), VaultError> {
        if !self.item_exists(item_id)? {
            return Err(VaultError::ItemNotFound);
        }
        journal::create_local_change(
            &mut self.db,
            self.vault_id,
            item_id,
            &self.ed25519,
            &self.dek,
            cipher::Operation::Tombstone,
            [],
            u32::from(ITEM_SCHEMA_VERSION),
            now_millis(),
        )?;
        Ok(())
    }

    /// List all non-deleted items and decode their winning payloads.
    pub fn list_items(&self) -> Result<Vec<(ItemId, ItemPayload)>, VaultError> {
        let mut statement = self.db.connection().prepare(LIVE_ITEMS_QUERY)?;
        let changes = statement
            .query_map([], journal::row_to_change)?
            .collect::<Result<Vec<_>, _>>()?;
        changes
            .into_iter()
            .map(|change| {
                let item_id = change.item_id;
                self.decode_payload(&change)
                    .map(|payload| (item_id, payload))
            })
            .collect()
    }

    /// Return one non-deleted item, or `None` for absent/tombstoned ids.
    pub fn get_item(&self, item_id: ItemId) -> Result<Option<ItemPayload>, VaultError> {
        let change = self
            .db
            .connection()
            .query_row(GET_ITEM_QUERY, [item_id.as_ref()], journal::row_to_change)
            .optional()?;
        change
            .map(|change| self.decode_payload(&change))
            .transpose()
    }

    /// List ids whose current projection is tombstoned.
    pub fn list_deleted_items(&self) -> Result<Vec<ItemId>, VaultError> {
        let mut statement = self
            .db
            .connection()
            .prepare("SELECT item_id FROM items WHERE deleted = 1")?;
        Ok(statement
            .query_map([], |row| {
                let bytes: [u8; 16] = row.get::<_, Vec<u8>>(0)?.try_into().map_err(|_| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        Type::Blob,
                        "invalid item id".into(),
                    )
                })?;
                Ok(ItemId::from_bytes(bytes))
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Return the latest upsert payload for an item, including deleted items.
    pub fn last_known_payload(&self, item_id: ItemId) -> Result<Option<ItemPayload>, VaultError> {
        let change = self
            .db
            .connection()
            .query_row(
                LAST_UPSERT_QUERY,
                [item_id.as_ref()],
                journal::row_to_change,
            )
            .optional()?;
        change
            .map(|change| self.decode_payload(&change))
            .transpose()
    }

    /// List all unresolved heads, retaining change ids and tombstone choices.
    #[allow(clippy::type_complexity)]
    pub fn list_conflicts(
        &self,
    ) -> Result<Vec<(ItemId, Vec<(ChangeId, Option<ItemPayload>)>)>, VaultError> {
        let mut statement = self
            .db
            .connection()
            .prepare("SELECT DISTINCT item_id FROM conflicts")?;
        let item_ids = statement
            .query_map([], |row| {
                let bytes: [u8; 16] = row.get::<_, Vec<u8>>(0)?.try_into().map_err(|_| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        Type::Blob,
                        "invalid item id".into(),
                    )
                })?;
                Ok(ItemId::from_bytes(bytes))
            })?
            .collect::<Result<Vec<_>, _>>()?;

        item_ids
            .into_iter()
            .map(|item_id| {
                let heads = merge::unresolved_heads(&self.db, item_id)?;
                let values = heads
                    .into_iter()
                    .map(|change| {
                        if change.is_tombstone() {
                            Ok((change.change_id, None))
                        } else {
                            self.decode_payload(&change)
                                .map(|payload| (change.change_id, Some(payload)))
                        }
                    })
                    .collect::<Result<Vec<_>, VaultError>>()?;
                Ok((item_id, values))
            })
            .collect()
    }

    /// Resolve an item conflict by carrying one selected revision forward.
    pub fn resolve_conflict(
        &mut self,
        item_id: ItemId,
        selected_change_id: ChangeId,
    ) -> Result<(), VaultError> {
        merge::resolve_conflicts(
            &mut self.db,
            self.vault_id,
            item_id,
            selected_change_id,
            &self.ed25519,
            &self.dek,
            now_millis(),
        )?;
        Ok(())
    }

    fn item_exists(&self, item_id: ItemId) -> Result<bool, VaultError> {
        Ok(self.db.connection().query_row(
            "SELECT EXISTS(SELECT 1 FROM items WHERE item_id = ?1)",
            [item_id.as_ref()],
            |row| row.get(0),
        )?)
    }

    fn decode_payload(&self, change: &Change) -> Result<ItemPayload, VaultError> {
        let decrypted = cipher::decrypt(
            &self.dek,
            &change.aad_context(),
            &change.encrypted_payload(),
        )?;
        let payload = ItemPayload::from_json_bytes(decrypted.as_bytes())?;
        if u32::from(payload.schema_version) != change.payload_schema_version {
            return Err(VaultError::SchemaVersionMismatch {
                envelope: change.payload_schema_version,
                payload: payload.schema_version,
            });
        }
        Ok(payload)
    }

    /// Return the vault identifier.
    #[must_use]
    pub fn vault_id(&self) -> VaultId {
        self.vault_id
    }

    /// Return the local device identifier.
    #[must_use]
    pub fn device_id(&self) -> DeviceId {
        self.device_id
    }
}

/// Return the platform's default Nox data directory without touching the filesystem.
pub fn default_data_dir() -> Result<PathBuf, VaultError> {
    #[cfg(target_os = "macos")]
    let base = {
        let home = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .ok_or(VaultError::NoHomeDirectory)?;
        PathBuf::from(home).join("Library/Application Support")
    };

    #[cfg(not(target_os = "macos"))]
    let base = match std::env::var_os("XDG_DATA_HOME").filter(|path| !path.is_empty()) {
        Some(path) => PathBuf::from(path),
        None => {
            let home = std::env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .ok_or(VaultError::NoHomeDirectory)?;
            PathBuf::from(home).join(".local/share")
        }
    };

    Ok(base.join("nox"))
}

/// Return the platform's default Nox vault path without touching the filesystem.
pub fn default_vault_path() -> Result<PathBuf, VaultError> {
    Ok(default_data_dir()?.join("vault.db"))
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or(std::time::Duration::ZERO)
        .as_millis() as u64
}

fn remove_new_database_files(path: &Path) {
    let _ = std::fs::remove_file(path);
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(sidecar));
    }
}

#[cfg(test)]
mod tests {
    use super::{Vault, VaultError, now_millis};
    use crate::{
        Ed25519Keypair, ITEM_SCHEMA_VERSION, IconChoice, ItemId, ItemPayload, ItemType, NoteColor,
        Operation,
        ids::ChangeId,
        journal::{self, JournalError},
        merge,
    };
    use std::{
        collections::BTreeMap,
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn unlock_restores_the_same_signing_key() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("locker-vault-signature-{unique}"));
        let path = directory.join("vault.db");
        let created = Vault::create(b"password", &path).unwrap();
        let signature = created.ed25519.sign(b"stable message");
        created.lock();

        let unlocked = Vault::unlock(b"password", &path).unwrap();
        assert_eq!(unlocked.ed25519.sign(b"stable message"), signature);
        unlocked.lock();
        fs::remove_dir_all(directory).unwrap();
    }

    fn payload(title: &str, password: &str) -> ItemPayload {
        ItemPayload {
            schema_version: ITEM_SCHEMA_VERSION,
            item_type: ItemType::Login,
            title: title.into(),
            username: "alice".into(),
            password: password.into(),
            uris: vec![],
            notes: String::new(),
            created_at: 1,
            updated_at: 2,
            icon: IconChoice::Default,
            note_color: NoteColor::Blue,
            note_tags: vec![],
        }
    }

    fn f5_directory(label: &str) -> PathBuf {
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).unwrap();
        let suffix = u128::from_le_bytes(random);
        std::env::temp_dir().join(format!("locker-vault-f5-{label}-{suffix:032x}"))
    }

    #[test]
    fn replication_store_uses_bounded_cursors_and_idempotent_batches() {
        let directory = f5_directory("replication-store");
        let path = directory.join("vault.db");
        let mut vault = Vault::create(b"password", &path).unwrap();
        vault
            .create_item(&payload("replication", "secret"))
            .unwrap();
        let mut store = vault.open_replication_store().unwrap();
        let changes = store
            .missing_changes(&BTreeMap::new(), 64, 60 * 1024)
            .unwrap();
        assert_eq!(changes.len(), 1);
        let authorization = store.authorization_snapshot().unwrap();
        let result = store
            .apply_remote_batch(vault.device_id, &authorization, &changes)
            .unwrap();
        assert_eq!(result.inserted, 0);
        assert_eq!(result.duplicates, 1);
        assert_eq!(store.cursor_map().unwrap().len(), 1);
        drop(store);
        drop(vault);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn sync_access_keeps_typed_secrets_redacted_and_opens_stores() {
        let directory = f5_directory("sync-access");
        let path = directory.join("vault.db");
        let vault = Vault::create(b"password", &path).unwrap();
        let access = vault.open_sync_access().unwrap();
        assert_eq!(access.vault_id(), vault.vault_id());
        assert_eq!(access.local_device_id(), vault.device_id());
        assert_eq!(access.local_identity().device_id, access.local_device_id());
        assert!(!format!("{access:?}").contains("password"));
        let _pairing = access.open_pairing_store().unwrap();
        let _replication = access.open_replication_store().unwrap();
        drop(access);
        vault.lock();
        fs::remove_dir_all(directory).unwrap();
    }

    fn assert_resolution_matches(
        vault: &Vault,
        item_id: ItemId,
        previous: &[journal::Change],
        heads: &[journal::Change],
        selected: &journal::Change,
    ) {
        let previous_ids = previous
            .iter()
            .map(|change| change.change_id)
            .collect::<Vec<_>>();
        let changes = journal::load_item_changes(vault.db.connection(), item_id).unwrap();
        let resolving = changes
            .iter()
            .filter(|change| !previous_ids.contains(&change.change_id))
            .collect::<Vec<_>>();
        assert_eq!(resolving.len(), 1);
        let resolving = resolving[0];

        assert!(
            heads
                .iter()
                .all(|head| head.change_id != resolving.change_id)
        );
        let mut expected_parents = heads.iter().map(|head| head.change_id).collect::<Vec<_>>();
        expected_parents.sort_unstable();
        assert_eq!(resolving.parent_change_ids, expected_parents);
        assert!(resolving.parent_change_ids.contains(&selected.change_id));
        assert_eq!(resolving.operation, selected.operation);
        assert_eq!(
            resolving.payload_schema_version,
            selected.payload_schema_version
        );

        let selected_plaintext = crate::crypto::cipher::decrypt(
            &vault.dek,
            &selected.aad_context(),
            &selected.encrypted_payload(),
        )
        .unwrap();
        let resolving_plaintext = crate::crypto::cipher::decrypt(
            &vault.dek,
            &resolving.aad_context(),
            &resolving.encrypted_payload(),
        )
        .unwrap();
        assert_eq!(
            selected_plaintext.as_bytes(),
            resolving_plaintext.as_bytes()
        );

        assert!(vault.list_conflicts().unwrap().is_empty());
        match selected.operation {
            Operation::Tombstone => {
                assert_eq!(vault.get_item(item_id).unwrap(), None);
                assert!(vault.list_deleted_items().unwrap().contains(&item_id));
            }
            Operation::Upsert => {
                let expected_payload = vault.decode_payload(selected).unwrap();
                assert_eq!(vault.get_item(item_id).unwrap(), Some(expected_payload));
                assert!(!vault.list_deleted_items().unwrap().contains(&item_id));
            }
        }
    }

    #[test]
    fn conflict_resolution_creates_all_parent_revision_and_converges() {
        for (label, selected_tombstone) in [("edit-edit", false), ("edit-tombstone", true)] {
            let directory = f5_directory(label);
            let path = directory.join("vault.db");
            let mut vault = Vault::create(b"password", &path).unwrap();
            let item_id = vault.create_item(&payload("base", "base-secret")).unwrap();
            let base = journal::load_item_changes(vault.db.connection(), item_id)
                .unwrap()
                .into_iter()
                .next()
                .unwrap();

            let local_payload = payload("local", "local-secret");
            vault.update_item(item_id, &local_payload).unwrap();
            let remote = Ed25519Keypair::from_private_bytes(if selected_tombstone {
                [46; 32]
            } else {
                [45; 32]
            });
            let remote_bytes = if selected_tombstone {
                Vec::new()
            } else {
                payload("remote", "remote-secret").to_json_bytes().unwrap()
            };
            let remote_change = journal::create_local_change_with_parents(
                &mut vault.db,
                vault.vault_id,
                item_id,
                vec![base.change_id],
                &remote,
                &vault.dek,
                if selected_tombstone {
                    Operation::Tombstone
                } else {
                    Operation::Upsert
                },
                remote_bytes,
                u32::from(ITEM_SCHEMA_VERSION),
                now_millis(),
            )
            .unwrap();

            let previous = journal::load_item_changes(vault.db.connection(), item_id).unwrap();
            let heads = merge::unresolved_heads(&vault.db, item_id).unwrap();
            assert_eq!(heads.len(), 2);
            assert!(
                heads
                    .iter()
                    .any(|head| head.change_id == remote_change.change_id)
            );
            let selected = if selected_tombstone {
                heads.iter().find(|head| head.is_tombstone()).unwrap()
            } else {
                heads
                    .iter()
                    .find(|head| head.operation == Operation::Upsert)
                    .unwrap()
            };
            vault.resolve_conflict(item_id, selected.change_id).unwrap();
            assert_resolution_matches(&vault, item_id, &previous, &heads, selected);

            vault.lock();
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn conflicts_return_selectable_change_ids_and_resolve() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("locker-vault-conflict-{unique}"));
        let path = directory.join("vault.db");
        let mut vault = Vault::create(b"password", &path).unwrap();
        let base = payload("base", "base-secret");
        let left = payload("left", "left-secret");
        let right = payload("right", "right-secret");
        let item_id = vault.create_item(&base).unwrap();
        let base_change = journal::load_item_changes(vault.db.connection(), item_id)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        vault.update_item(item_id, &left).unwrap();
        let remote = Ed25519Keypair::from_private_bytes([42; 32]);
        let right_bytes = right.to_json_bytes().unwrap();
        journal::create_local_change_with_parents(
            &mut vault.db,
            vault.vault_id,
            item_id,
            vec![base_change.change_id],
            &remote,
            &vault.dek,
            Operation::Upsert,
            right_bytes,
            u32::from(ITEM_SCHEMA_VERSION),
            now_millis(),
        )
        .unwrap();

        let conflicts = vault.list_conflicts().unwrap();
        assert_eq!(conflicts.len(), 1);
        assert!(matches!(
            vault.resolve_conflict(item_id, ChangeId::new()),
            Err(VaultError::Journal(JournalError::InvalidChange(_)))
        ));
        assert_eq!(vault.list_conflicts().unwrap(), conflicts);
        let (selected, expected) = conflicts[0]
            .1
            .iter()
            .find_map(|(change_id, payload)| payload.clone().map(|payload| (*change_id, payload)))
            .unwrap();
        vault.resolve_conflict(item_id, selected).unwrap();
        assert!(vault.list_conflicts().unwrap().is_empty());
        assert_eq!(vault.get_item(item_id).unwrap(), Some(expected));

        vault.lock();
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn tombstone_conflict_head_is_visible_and_selectable() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("locker-vault-tombstone-{unique}"));
        let path = directory.join("vault.db");
        let mut vault = Vault::create(b"password", &path).unwrap();
        let item_id = vault.create_item(&payload("base", "base-secret")).unwrap();
        let base_change = journal::load_item_changes(vault.db.connection(), item_id)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        vault
            .update_item(item_id, &payload("edit", "edit-secret"))
            .unwrap();
        let remote = Ed25519Keypair::from_private_bytes([43; 32]);
        journal::create_local_change_with_parents(
            &mut vault.db,
            vault.vault_id,
            item_id,
            vec![base_change.change_id],
            &remote,
            &vault.dek,
            Operation::Tombstone,
            [],
            u32::from(ITEM_SCHEMA_VERSION),
            now_millis(),
        )
        .unwrap();

        let conflicts = vault.list_conflicts().unwrap();
        let tombstone_id = conflicts[0]
            .1
            .iter()
            .find_map(|(change_id, payload)| payload.is_none().then_some(*change_id))
            .unwrap();
        vault.resolve_conflict(item_id, tombstone_id).unwrap();
        assert_eq!(vault.get_item(item_id).unwrap(), None);
        assert!(vault.list_conflicts().unwrap().is_empty());

        vault.lock();
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn edit_conflict_head_is_visible_and_selectable() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("locker-vault-edit-head-{unique}"));
        let path = directory.join("vault.db");
        let mut vault = Vault::create(b"password", &path).unwrap();
        let base = payload("base", "base-secret");
        let edit = payload("edit", "edit-secret");
        let item_id = vault.create_item(&base).unwrap();
        let base_change = journal::load_item_changes(vault.db.connection(), item_id)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        vault.update_item(item_id, &edit).unwrap();
        let remote = Ed25519Keypair::from_private_bytes([44; 32]);
        journal::create_local_change_with_parents(
            &mut vault.db,
            vault.vault_id,
            item_id,
            vec![base_change.change_id],
            &remote,
            &vault.dek,
            Operation::Tombstone,
            [],
            u32::from(ITEM_SCHEMA_VERSION),
            now_millis(),
        )
        .unwrap();

        let conflicts = vault.list_conflicts().unwrap();
        let (edit_id, selected_edit) = conflicts[0]
            .1
            .iter()
            .find_map(|(change_id, payload)| payload.clone().map(|payload| (*change_id, payload)))
            .unwrap();
        assert_eq!(selected_edit, edit);
        vault.resolve_conflict(item_id, edit_id).unwrap();
        assert_eq!(vault.get_item(item_id).unwrap(), Some(edit));
        assert!(vault.list_conflicts().unwrap().is_empty());

        vault.lock();
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn envelope_and_payload_schema_mismatch_is_reported_honestly() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("locker-vault-schema-{unique}"));
        let path = directory.join("vault.db");
        let mut vault = Vault::create(b"password", &path).unwrap();
        let item_id = vault.create_item(&payload("item", "secret")).unwrap();
        let mismatched = payload("mismatch", "secret").to_json_bytes().unwrap();
        journal::create_local_change(
            &mut vault.db,
            vault.vault_id,
            item_id,
            &vault.ed25519,
            &vault.dek,
            Operation::Upsert,
            mismatched,
            2,
            now_millis(),
        )
        .unwrap();
        assert!(matches!(
            vault.get_item(item_id),
            Err(VaultError::SchemaVersionMismatch {
                envelope: 2,
                payload: ITEM_SCHEMA_VERSION,
            })
        ));
        vault.lock();
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn corrupted_winning_ciphertext_aborts_item_reads() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("locker-vault-corrupt-item-{unique}"));
        let path = directory.join("vault.db");
        let mut vault = Vault::create(b"password", &path).unwrap();
        let item_id = vault.create_item(&payload("item", "secret")).unwrap();
        let mut ciphertext: Vec<u8> = vault
            .db
            .connection()
            .query_row(
                "SELECT ciphertext FROM changes WHERE item_id = ?1",
                [item_id.as_ref()],
                |row| row.get(0),
            )
            .unwrap();
        ciphertext[0] ^= 1;
        vault
            .db
            .connection()
            .execute(
                "UPDATE changes SET ciphertext = ?1 WHERE item_id = ?2",
                rusqlite::params![ciphertext, item_id.as_ref()],
            )
            .unwrap();
        assert!(matches!(
            vault.list_items(),
            Err(VaultError::Cipher(
                crate::crypto::CipherError::InvalidCiphertext
            ))
        ));
        vault.lock();
        fs::remove_dir_all(directory).unwrap();
    }
}
