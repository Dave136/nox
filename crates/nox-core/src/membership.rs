//! Canonical, signed vault membership and local authorization state.

use crate::{
    DeviceId, Ed25519Keypair, Hlc, VaultId,
    crypto::keys,
    storage::{Db, DbError},
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

pub const MEMBERSHIP_FORMAT_VERSION: u16 = 1;
pub const MAX_DEVICE_DISPLAY_NAME_BYTES: usize = 128;
pub const MAX_MEMBERSHIP_RECORD_BYTES: usize = 4 * 1024;
pub const MAX_MEMBERSHIP_RECORDS: usize = 1024;
pub const MAX_ACTIVE_MEMBERS: usize = 256;

const MEMBERSHIP_DOMAIN: &[u8] = b"NOX-MEMBERSHIP\0";
const IDENTITY_DOMAIN: &[u8] = b"NOX-DEVICE-IDENTITY\0";
const HASH_DOMAIN: &[u8] = b"NOX-MEMBERSHIP-HASH\0";

#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MembershipRecordHash([u8; 32]);

impl MembershipRecordHash {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for MembershipRecordHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MembershipRecordHash(<redacted>)")
    }
}

impl AsRef<[u8]> for MembershipRecordHash {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceIdentity {
    pub device_id: DeviceId,
    pub ed25519_public_key: [u8; 32],
    pub x25519_public_key: [u8; 32],
    pub display_name: String,
    pub protocol_version: u16,
    pub self_signature: [u8; 64],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MembershipGenesis {
    pub vault_id: VaultId,
    pub creator: DeviceIdentity,
    pub created_at: Hlc,
    pub signature: [u8; 64],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MembershipAdmission {
    pub vault_id: VaultId,
    pub admitted_device: DeviceIdentity,
    pub invited_by: DeviceId,
    pub admitted_at: Hlc,
    pub signature: [u8; 64],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MembershipAcceptance {
    pub vault_id: VaultId,
    pub admission_hash: MembershipRecordHash,
    pub admitted_device_id: DeviceId,
    pub accepted_at: Hlc,
    pub signature: [u8; 64],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MembershipRecord {
    Genesis(MembershipGenesis),
    Admission(MembershipAdmission),
    Acceptance(MembershipAcceptance),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemberPublicKeys {
    pub device_id: DeviceId,
    pub ed25519: [u8; 32],
    pub x25519: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MembershipInsert {
    Inserted,
    AlreadyPresent,
}

#[derive(Debug)]
pub enum MembershipError {
    Encoding,
    TooManyRecords,
    RecordTooLarge,
    InvalidIdentity(&'static str),
    InvalidRecord(&'static str),
    WrongVault,
    InvalidSignature,
    MissingGenesis,
    MultipleGenesis,
    OrphanAcceptance,
    UnchainedAdmission,
    ConflictingIdentity,
    ConflictingRecord,
    MemberLimitExceeded,
    CannotBlockLocalDevice,
    UnknownDevice,
    Storage(DbError),
}

impl fmt::Display for MembershipError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::Encoding => "membership encoding failed",
            Self::TooManyRecords => "too many membership records",
            Self::RecordTooLarge => "membership record is too large",
            Self::InvalidIdentity(_) => "invalid membership identity",
            Self::InvalidRecord(_) => "invalid membership record",
            Self::WrongVault => "membership belongs to another vault",
            Self::InvalidSignature => "invalid membership signature",
            Self::MissingGenesis => "membership genesis is missing",
            Self::MultipleGenesis => "membership has multiple genesis records",
            Self::OrphanAcceptance => "membership acceptance has no admission",
            Self::UnchainedAdmission => "membership admission is not chained",
            Self::ConflictingIdentity => "membership identity conflicts",
            Self::ConflictingRecord => "membership record conflicts",
            Self::MemberLimitExceeded => "membership member limit exceeded",
            Self::CannotBlockLocalDevice => "cannot block the local device",
            Self::UnknownDevice => "unknown membership device",
            Self::Storage(_) => "membership storage error",
        };
        f.write_str(text)
    }
}

impl std::error::Error for MembershipError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage(e) => Some(e),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for MembershipError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(DbError::Sql(error))
    }
}

impl DeviceIdentity {
    pub fn new_signed(
        display_name: &str,
        protocol_version: u16,
        ed25519: &Ed25519Keypair,
        x25519_public_key: [u8; 32],
    ) -> Result<Self, MembershipError> {
        let device_id = DeviceId::from_public_key(ed25519.public_key_bytes());
        let mut identity = Self {
            device_id,
            ed25519_public_key: ed25519.public_key_bytes(),
            x25519_public_key,
            display_name: display_name.to_owned(),
            protocol_version,
            self_signature: [0; 64],
        };
        identity.validate_shape()?;
        identity.self_signature = ed25519.sign(&identity.signed_bytes()?);
        Ok(identity)
    }

    pub fn verify(&self) -> Result<(), MembershipError> {
        self.validate_shape()?;
        keys::verify_signature(
            self.ed25519_public_key,
            &self.signed_bytes()?,
            self.self_signature,
        )
        .map_err(|_| MembershipError::InvalidSignature)
    }

    pub fn signed_bytes(&self) -> Result<Vec<u8>, MembershipError> {
        self.validate_shape()?;
        let mut out = Vec::with_capacity(
            2 + 32 + 32 + 32 + 2 + self.display_name.len() + 2 + IDENTITY_DOMAIN.len(),
        );
        out.extend_from_slice(IDENTITY_DOMAIN);
        put_u16(&mut out, MEMBERSHIP_FORMAT_VERSION);
        out.extend_from_slice(self.device_id.as_bytes());
        out.extend_from_slice(&self.ed25519_public_key);
        out.extend_from_slice(&self.x25519_public_key);
        put_bytes(&mut out, self.display_name.as_bytes())?;
        put_u16(&mut out, self.protocol_version);
        Ok(out)
    }

    fn validate_shape(&self) -> Result<(), MembershipError> {
        if self.device_id != DeviceId::from_public_key(self.ed25519_public_key) {
            return Err(MembershipError::InvalidIdentity("device id"));
        }
        if self.display_name.trim().is_empty()
            || self.display_name.len() > MAX_DEVICE_DISPLAY_NAME_BYTES
            || self.display_name.chars().any(char::is_control)
        {
            return Err(MembershipError::InvalidIdentity("display name"));
        }
        if self.protocol_version != MEMBERSHIP_FORMAT_VERSION {
            return Err(MembershipError::InvalidIdentity("protocol version"));
        }
        Ok(())
    }
}

impl MembershipRecord {
    pub fn signed_bytes(&self) -> Result<Vec<u8>, MembershipError> {
        let mut out = Vec::with_capacity(MAX_MEMBERSHIP_RECORD_BYTES.min(512));
        out.extend_from_slice(MEMBERSHIP_DOMAIN);
        put_u16(&mut out, MEMBERSHIP_FORMAT_VERSION);
        match self {
            Self::Genesis(value) => {
                out.push(1);
                out.extend_from_slice(value.vault_id.as_bytes());
                append_identity(&mut out, &value.creator)?;
                put_hlc(&mut out, value.created_at);
            }
            Self::Admission(value) => {
                out.push(2);
                out.extend_from_slice(value.vault_id.as_bytes());
                append_identity(&mut out, &value.admitted_device)?;
                out.extend_from_slice(value.invited_by.as_bytes());
                put_hlc(&mut out, value.admitted_at);
            }
            Self::Acceptance(value) => {
                out.push(3);
                out.extend_from_slice(value.vault_id.as_bytes());
                out.extend_from_slice(value.admission_hash.as_bytes());
                out.extend_from_slice(value.admitted_device_id.as_bytes());
                put_hlc(&mut out, value.accepted_at);
            }
        }
        if out.len() > MAX_MEMBERSHIP_RECORD_BYTES - 64 {
            return Err(MembershipError::RecordTooLarge);
        }
        Ok(out)
    }

    pub fn to_canonical_bytes(&self) -> Result<Vec<u8>, MembershipError> {
        let mut out = self.signed_bytes()?;
        let signature = match self {
            Self::Genesis(v) => v.signature,
            Self::Admission(v) => v.signature,
            Self::Acceptance(v) => v.signature,
        };
        out.extend_from_slice(&signature);
        Ok(out)
    }

    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self, MembershipError> {
        if bytes.len() > MAX_MEMBERSHIP_RECORD_BYTES {
            return Err(MembershipError::RecordTooLarge);
        }
        let mut cursor = Cursor::new(bytes);
        if cursor.take(MEMBERSHIP_DOMAIN.len())? != MEMBERSHIP_DOMAIN {
            return Err(MembershipError::Encoding);
        }
        if cursor.u16()? != MEMBERSHIP_FORMAT_VERSION {
            return Err(MembershipError::InvalidRecord("version"));
        }
        let tag = cursor.byte()?;
        let record = match tag {
            1 => Self::Genesis(MembershipGenesis {
                vault_id: VaultId::from_bytes(cursor.array()?),
                creator: parse_identity(&mut cursor)?,
                created_at: cursor.hlc()?,
                signature: cursor.array()?,
            }),
            2 => Self::Admission(MembershipAdmission {
                vault_id: VaultId::from_bytes(cursor.array()?),
                admitted_device: parse_identity(&mut cursor)?,
                invited_by: DeviceId::from_bytes(cursor.array()?),
                admitted_at: cursor.hlc()?,
                signature: cursor.array()?,
            }),
            3 => Self::Acceptance(MembershipAcceptance {
                vault_id: VaultId::from_bytes(cursor.array()?),
                admission_hash: MembershipRecordHash::from_bytes(cursor.array()?),
                admitted_device_id: DeviceId::from_bytes(cursor.array()?),
                accepted_at: cursor.hlc()?,
                signature: cursor.array()?,
            }),
            _ => return Err(MembershipError::InvalidRecord("tag")),
        };
        if !cursor.is_empty() {
            return Err(MembershipError::Encoding);
        }
        Ok(record)
    }

    pub fn record_hash(&self) -> Result<MembershipRecordHash, MembershipError> {
        let bytes = self.to_canonical_bytes()?;
        let mut hasher = Sha256::new();
        hasher.update(HASH_DOMAIN);
        hasher.update(bytes);
        Ok(MembershipRecordHash::from_bytes(hasher.finalize().into()))
    }

    #[must_use]
    pub fn vault_id(&self) -> VaultId {
        match self {
            Self::Genesis(v) => v.vault_id,
            Self::Admission(v) => v.vault_id,
            Self::Acceptance(v) => v.vault_id,
        }
    }
}

pub fn create_genesis(
    vault_id: VaultId,
    creator: DeviceIdentity,
    created_at: Hlc,
    creator_key: &Ed25519Keypair,
) -> Result<MembershipRecord, MembershipError> {
    creator.verify()?;
    if creator.device_id != DeviceId::from_public_key(creator_key.public_key_bytes()) {
        return Err(MembershipError::InvalidIdentity("creator key"));
    }
    let mut record = MembershipRecord::Genesis(MembershipGenesis {
        vault_id,
        creator,
        created_at,
        signature: [0; 64],
    });
    let signed = record.signed_bytes()?;
    if let MembershipRecord::Genesis(value) = &mut record {
        value.signature = creator_key.sign(&signed);
    }
    Ok(record)
}

pub fn create_admission(
    vault_id: VaultId,
    admitted_device: DeviceIdentity,
    invited_by: DeviceId,
    admitted_at: Hlc,
    inviter_key: &Ed25519Keypair,
) -> Result<MembershipRecord, MembershipError> {
    admitted_device.verify()?;
    if invited_by != DeviceId::from_public_key(inviter_key.public_key_bytes()) {
        return Err(MembershipError::InvalidIdentity("inviter key"));
    }
    let mut record = MembershipRecord::Admission(MembershipAdmission {
        vault_id,
        admitted_device,
        invited_by,
        admitted_at,
        signature: [0; 64],
    });
    let signed = record.signed_bytes()?;
    if let MembershipRecord::Admission(value) = &mut record {
        value.signature = inviter_key.sign(&signed);
    }
    Ok(record)
}

pub fn create_acceptance(
    vault_id: VaultId,
    admission_hash: MembershipRecordHash,
    admitted_device_id: DeviceId,
    accepted_at: Hlc,
    admitted_key: &Ed25519Keypair,
) -> Result<MembershipRecord, MembershipError> {
    if admitted_device_id != DeviceId::from_public_key(admitted_key.public_key_bytes()) {
        return Err(MembershipError::InvalidIdentity("admitted key"));
    }
    let mut record = MembershipRecord::Acceptance(MembershipAcceptance {
        vault_id,
        admission_hash,
        admitted_device_id,
        accepted_at,
        signature: [0; 64],
    });
    let signed = record.signed_bytes()?;
    if let MembershipRecord::Acceptance(value) = &mut record {
        value.signature = admitted_key.sign(&signed);
    }
    Ok(record)
}

pub struct ValidatedMembership {
    vault_id: VaultId,
    genesis: MembershipGenesis,
    records: Vec<MembershipRecord>,
    record_index: BTreeMap<MembershipRecordHash, usize>,
    identities: BTreeMap<DeviceId, DeviceIdentity>,
    active: BTreeMap<DeviceId, MemberPublicKeys>,
    active_identities: BTreeMap<DeviceId, DeviceIdentity>,
    pending: BTreeSet<DeviceId>,
}

impl ValidatedMembership {
    #[must_use]
    pub fn vault_id(&self) -> VaultId {
        self.vault_id
    }
    #[must_use]
    pub fn genesis(&self) -> &MembershipGenesis {
        &self.genesis
    }
    #[must_use]
    pub fn is_active(&self, id: DeviceId) -> bool {
        self.active.contains_key(&id)
    }
    #[must_use]
    pub fn active_member_count(&self) -> usize {
        self.active.len()
    }
    #[must_use]
    pub fn member_keys(&self, id: DeviceId) -> Option<&MemberPublicKeys> {
        self.active.get(&id)
    }
    pub fn active_members(&self) -> impl ExactSizeIterator<Item = &DeviceIdentity> {
        self.active_identities.values()
    }
    pub fn records(&self) -> impl ExactSizeIterator<Item = &MembershipRecord> {
        self.records.iter()
    }
    /// Return a validated record by its canonical hash.
    pub fn record(&self, hash: MembershipRecordHash) -> Option<&MembershipRecord> {
        self.record_index
            .get(&hash)
            .and_then(|index| self.records.get(*index))
    }
    /// Return the immutable identity index entry for a known device.
    pub fn identity(&self, id: DeviceId) -> Option<&DeviceIdentity> {
        self.identities.get(&id)
    }
    fn is_known(&self, id: DeviceId) -> bool {
        self.active.contains_key(&id) || self.pending.contains(&id)
    }
}

pub struct AuthorizationSnapshot {
    pub membership: ValidatedMembership,
    blocked: BTreeSet<DeviceId>,
}

impl AuthorizationSnapshot {
    #[must_use]
    pub fn is_current_member(&self, id: DeviceId) -> bool {
        self.membership.is_active(id)
    }
    pub fn authorize_peer(&self, id: DeviceId) -> Result<&MemberPublicKeys, MembershipError> {
        if self.is_locally_blocked(id) {
            return Err(MembershipError::UnknownDevice);
        }
        self.membership
            .member_keys(id)
            .ok_or(MembershipError::UnknownDevice)
    }
    #[must_use]
    pub fn is_locally_blocked(&self, id: DeviceId) -> bool {
        self.blocked.contains(&id)
    }
}

pub fn validate_membership(
    expected_vault_id: VaultId,
    records: &[MembershipRecord],
) -> Result<ValidatedMembership, MembershipError> {
    if records.is_empty() {
        return Err(MembershipError::MissingGenesis);
    }
    if records.len() > MAX_MEMBERSHIP_RECORDS {
        return Err(MembershipError::TooManyRecords);
    }
    let mut by_hash = BTreeMap::<MembershipRecordHash, (Vec<u8>, MembershipRecord)>::new();
    let mut identities = BTreeMap::<DeviceId, DeviceIdentity>::new();
    let mut genesis = None;
    let mut admissions = Vec::new();
    let mut accepts = BTreeMap::<MembershipRecordHash, Vec<MembershipAcceptance>>::new();
    for record in records {
        if record.vault_id() != expected_vault_id {
            return Err(MembershipError::WrongVault);
        }
        let bytes = record.to_canonical_bytes()?;
        if bytes.len() > MAX_MEMBERSHIP_RECORD_BYTES {
            return Err(MembershipError::RecordTooLarge);
        }
        let hash = record.record_hash()?;
        if let Some((old, _)) = by_hash.get(&hash) {
            if old != &bytes {
                return Err(MembershipError::ConflictingRecord);
            }
            continue;
        }
        match record {
            MembershipRecord::Genesis(value) => {
                if genesis.is_some() {
                    return Err(MembershipError::MultipleGenesis);
                }
                value.creator.verify()?;
                verify_outer(record)?;
                add_identity(&mut identities, &value.creator)?;
                genesis = Some(value.clone());
            }
            MembershipRecord::Admission(value) => {
                value.admitted_device.verify()?;
                add_identity(&mut identities, &value.admitted_device)?;
                admissions.push((hash, value.clone()));
            }
            MembershipRecord::Acceptance(value) => {
                accepts
                    .entry(value.admission_hash)
                    .or_default()
                    .push(value.clone());
            }
        }
        by_hash.insert(hash, (bytes, record.clone()));
    }
    let genesis = genesis.ok_or(MembershipError::MissingGenesis)?;
    let mut admission_by_hash = BTreeMap::new();
    for (hash, admission) in admissions {
        admission_by_hash.insert(hash, admission);
    }
    let mut accepted_for = BTreeMap::new();
    let mut accepted_devices = BTreeSet::new();
    for (hash, values) in accepts {
        let admission = admission_by_hash
            .get(&hash)
            .ok_or(MembershipError::OrphanAcceptance)?;
        if values.len() != 1 {
            return Err(MembershipError::ConflictingRecord);
        }
        let acceptance = values
            .into_iter()
            .next()
            .ok_or(MembershipError::OrphanAcceptance)?;
        if acceptance.vault_id != expected_vault_id
            || acceptance.admitted_device_id != admission.admitted_device.device_id
        {
            return Err(MembershipError::InvalidRecord("acceptance target"));
        }
        if !accepted_devices.insert(acceptance.admitted_device_id) {
            return Err(MembershipError::ConflictingRecord);
        }
        accepted_for.insert(hash, acceptance);
    }
    let creator_id = genesis.creator.device_id;
    let creator_keys = MemberPublicKeys {
        device_id: creator_id,
        ed25519: genesis.creator.ed25519_public_key,
        x25519: genesis.creator.x25519_public_key,
    };
    let mut active = BTreeMap::from([(creator_id, creator_keys)]);
    let mut active_identities = BTreeMap::from([(creator_id, genesis.creator.clone())]);
    let mut pending = BTreeSet::new();
    let mut remaining = admission_by_hash;
    loop {
        let before = remaining.len();
        let hashes: Vec<_> = remaining.keys().copied().collect();
        for hash in hashes {
            let Some(admission) = remaining.get(&hash) else {
                continue;
            };
            if admission.invited_by == admission.admitted_device.device_id
                || !active.contains_key(&admission.invited_by)
            {
                continue;
            }
            verify_admission(
                &MembershipRecord::Admission(admission.clone()),
                active
                    .get(&admission.invited_by)
                    .ok_or(MembershipError::UnchainedAdmission)?
                    .ed25519,
            )?;
            if let Some(acceptance) = accepted_for.get(&hash) {
                verify_acceptance(acceptance, &admission.admitted_device)?;
                if active.contains_key(&admission.admitted_device.device_id) {
                    return Err(MembershipError::ConflictingRecord);
                }
                if active.len() >= MAX_ACTIVE_MEMBERS {
                    return Err(MembershipError::MemberLimitExceeded);
                }
                let id = admission.admitted_device.device_id;
                active.insert(
                    id,
                    MemberPublicKeys {
                        device_id: id,
                        ed25519: admission.admitted_device.ed25519_public_key,
                        x25519: admission.admitted_device.x25519_public_key,
                    },
                );
                active_identities.insert(id, admission.admitted_device.clone());
            } else {
                pending.insert(admission.admitted_device.device_id);
            }
            remaining.remove(&hash);
        }
        if remaining.len() == before {
            break;
        }
    }
    if !remaining.is_empty() {
        return Err(MembershipError::UnchainedAdmission);
    }
    let mut sorted_records: Vec<_> = by_hash.into_values().map(|(_, record)| record).collect();
    sorted_records.sort_by_key(|record| record.record_hash().ok());
    let record_index = sorted_records
        .iter()
        .enumerate()
        .filter_map(|(index, record)| record.record_hash().ok().map(|hash| (hash, index)))
        .collect();
    Ok(ValidatedMembership {
        vault_id: expected_vault_id,
        genesis,
        records: sorted_records,
        record_index,
        identities,
        active,
        active_identities,
        pending,
    })
}

fn add_identity(
    index: &mut BTreeMap<DeviceId, DeviceIdentity>,
    identity: &DeviceIdentity,
) -> Result<(), MembershipError> {
    if let Some(old) = index.get(&identity.device_id) {
        if old != identity {
            return Err(MembershipError::ConflictingIdentity);
        }
    } else {
        index.insert(identity.device_id, identity.clone());
    }
    Ok(())
}

fn verify_outer(record: &MembershipRecord) -> Result<(), MembershipError> {
    let (key, signature) = match record {
        MembershipRecord::Genesis(value) => (value.creator.ed25519_public_key, value.signature),
        MembershipRecord::Admission(_) => return Err(MembershipError::InvalidSignature),
        MembershipRecord::Acceptance(_) => return Err(MembershipError::InvalidSignature),
    };
    keys::verify_signature(key, &record.signed_bytes()?, signature)
        .map_err(|_| MembershipError::InvalidSignature)
}

fn verify_admission(record: &MembershipRecord, inviter: [u8; 32]) -> Result<(), MembershipError> {
    let signature = match record {
        MembershipRecord::Admission(value) => value.signature,
        _ => return Err(MembershipError::InvalidRecord("admission")),
    };
    keys::verify_signature(inviter, &record.signed_bytes()?, signature)
        .map_err(|_| MembershipError::InvalidSignature)
}

fn verify_acceptance(
    value: &MembershipAcceptance,
    identity: &DeviceIdentity,
) -> Result<(), MembershipError> {
    let record = MembershipRecord::Acceptance(value.clone());
    keys::verify_signature(
        identity.ed25519_public_key,
        &record.signed_bytes()?,
        value.signature,
    )
    .map_err(|_| MembershipError::InvalidSignature)
}

pub fn load_membership(db: &Db, vault_id: VaultId) -> Result<ValidatedMembership, MembershipError> {
    load_membership_connection(db.connection(), vault_id)
}

/// Load and validate membership through an existing transaction snapshot.
/// Keeping the read on the transaction is required for atomic onboarding
/// acceptance: validation and insertion observe one SQLite snapshot.
pub(crate) fn load_membership_tx(
    tx: &Transaction<'_>,
    vault_id: VaultId,
) -> Result<ValidatedMembership, MembershipError> {
    load_membership_connection(tx, vault_id)
}

fn load_membership_connection(
    connection: &Connection,
    vault_id: VaultId,
) -> Result<ValidatedMembership, MembershipError> {
    let mut statement = connection.prepare(
        "SELECT record_hash, vault_id, record_type, record FROM memberships WHERE vault_id = ?1",
    )?;
    let rows = statement.query_map([vault_id.as_ref()], |row| {
        let hash: Vec<u8> = row.get(0)?;
        let stored_vault: Vec<u8> = row.get(1)?;
        let record_type: String = row.get(2)?;
        let bytes: Vec<u8> = row.get(3)?;
        Ok((hash, stored_vault, record_type, bytes))
    })?;
    let mut records = Vec::new();
    for row in rows {
        let (hash, stored_vault, record_type, bytes) = row?;
        if hash.len() != 32 || stored_vault.len() != 16 {
            return Err(MembershipError::Storage(DbError::Sql(
                rusqlite::Error::InvalidQuery,
            )));
        }
        if bytes.len() > MAX_MEMBERSHIP_RECORD_BYTES {
            return Err(MembershipError::RecordTooLarge);
        }
        let record = MembershipRecord::from_canonical_bytes(&bytes)?;
        if record.vault_id() != vault_id || stored_vault.as_slice() != vault_id.as_bytes() {
            return Err(MembershipError::WrongVault);
        }
        let hash = MembershipRecordHash::from_bytes(
            hash.try_into().map_err(|_| MembershipError::Encoding)?,
        );
        if record.record_hash()? != hash || record_type != record_type_for(&record) {
            return Err(MembershipError::ConflictingRecord);
        }
        records.push(record);
        if records.len() > MAX_MEMBERSHIP_RECORDS {
            return Err(MembershipError::TooManyRecords);
        }
    }
    validate_membership(vault_id, &records)
}

pub fn insert_membership_record(
    db: &mut Db,
    record: &MembershipRecord,
) -> Result<MembershipInsert, MembershipError> {
    db.transaction(|tx| insert_membership_record_tx(tx, record))
}

pub(crate) fn insert_membership_record_tx(
    tx: &Transaction<'_>,
    record: &MembershipRecord,
) -> Result<MembershipInsert, MembershipError> {
    let bytes = record.to_canonical_bytes()?;
    if bytes.len() > MAX_MEMBERSHIP_RECORD_BYTES {
        return Err(MembershipError::RecordTooLarge);
    }
    let hash = record.record_hash()?;
    let existing: Option<(Vec<u8>, Vec<u8>, String)> = tx
        .query_row(
            "SELECT vault_id, record, record_type FROM memberships WHERE record_hash = ?1",
            [hash.as_ref()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    if let Some((vault, old, kind)) = existing {
        if vault.as_slice() == record.vault_id().as_ref()
            && old == bytes
            && kind == record_type_for(record)
        {
            return Ok(MembershipInsert::AlreadyPresent);
        }
        return Err(MembershipError::ConflictingRecord);
    }
    tx.execute("INSERT INTO memberships (record_hash, vault_id, record_type, record) VALUES (?1, ?2, ?3, ?4)", params![hash.as_ref(), record.vault_id().as_ref(), record_type_for(record), bytes])?;
    Ok(MembershipInsert::Inserted)
}

pub fn block_device(
    db: &mut Db,
    local_device_id: DeviceId,
    target: DeviceId,
    blocked_at: Hlc,
) -> Result<(), MembershipError> {
    if local_device_id == target {
        return Err(MembershipError::CannotBlockLocalDevice);
    }
    let vault_id = db
        .connection()
        .query_row("SELECT vault_id FROM vault_meta LIMIT 1", [], |row| {
            let bytes: Vec<u8> = row.get(0)?;
            bytes
                .try_into()
                .map(VaultId::from_bytes)
                .map_err(|_| rusqlite::Error::InvalidQuery)
        })
        .optional()?
        .or(db
            .connection()
            .query_row("SELECT vault_id FROM memberships LIMIT 1", [], |row| {
                let bytes: Vec<u8> = row.get(0)?;
                bytes
                    .try_into()
                    .map(VaultId::from_bytes)
                    .map_err(|_| rusqlite::Error::InvalidQuery)
            })
            .optional()?)
        .ok_or(MembershipError::UnknownDevice)?;
    let membership = load_membership(db, vault_id)?;
    if !membership.is_known(target) {
        return Err(MembershipError::UnknownDevice);
    }
    let physical =
        i64::try_from(blocked_at.physical_ms).map_err(|_| MembershipError::InvalidRecord("HLC"))?;
    db.transaction(|tx| {
        tx.execute("INSERT INTO blocked_devices (device_id, blocked_at_physical_ms, blocked_at_logical) VALUES (?1, ?2, ?3) ON CONFLICT(device_id) DO UPDATE SET blocked_at_physical_ms = excluded.blocked_at_physical_ms, blocked_at_logical = excluded.blocked_at_logical WHERE excluded.blocked_at_physical_ms > blocked_devices.blocked_at_physical_ms OR (excluded.blocked_at_physical_ms = blocked_devices.blocked_at_physical_ms AND excluded.blocked_at_logical > blocked_devices.blocked_at_logical)", params![target.as_ref(), physical, i64::from(blocked_at.logical)])?;
        Ok(())
    })
}

pub fn unblock_device(db: &mut Db, target: DeviceId) -> Result<bool, MembershipError> {
    db.transaction(|tx| {
        Ok(tx.execute(
            "DELETE FROM blocked_devices WHERE device_id = ?1",
            [target.as_ref()],
        )? != 0)
    })
}

pub fn load_authorization(
    db: &Db,
    vault_id: VaultId,
) -> Result<AuthorizationSnapshot, MembershipError> {
    let membership = load_membership(db, vault_id)?;
    let mut blocked = BTreeSet::new();
    let mut statement = db
        .connection()
        .prepare("SELECT device_id FROM blocked_devices")?;
    for row in statement.query_map([], |row| row.get::<_, Vec<u8>>(0))? {
        let bytes = row?;
        let id: [u8; 32] = bytes
            .try_into()
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        blocked.insert(DeviceId::from_bytes(id));
    }
    Ok(AuthorizationSnapshot {
        membership,
        blocked,
    })
}

fn record_type_for(record: &MembershipRecord) -> &'static str {
    match record {
        MembershipRecord::Genesis(_) => "genesis",
        MembershipRecord::Admission(_) => "admission",
        MembershipRecord::Acceptance(_) => "acceptance",
    }
}

fn append_identity(out: &mut Vec<u8>, identity: &DeviceIdentity) -> Result<(), MembershipError> {
    identity.verify()?;
    out.extend_from_slice(&identity.signed_bytes()?);
    out.extend_from_slice(&identity.self_signature);
    Ok(())
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}
fn put_hlc(out: &mut Vec<u8>, value: Hlc) {
    out.extend_from_slice(&value.physical_ms.to_le_bytes());
    out.extend_from_slice(&value.logical.to_le_bytes());
}
fn put_bytes(out: &mut Vec<u8>, value: &[u8]) -> Result<(), MembershipError> {
    let len = u16::try_from(value.len()).map_err(|_| MembershipError::RecordTooLarge)?;
    put_u16(out, len);
    out.extend_from_slice(value);
    Ok(())
}

fn parse_identity(cursor: &mut Cursor<'_>) -> Result<DeviceIdentity, MembershipError> {
    if cursor.take(IDENTITY_DOMAIN.len())? != IDENTITY_DOMAIN {
        return Err(MembershipError::Encoding);
    }
    if cursor.u16()? != MEMBERSHIP_FORMAT_VERSION {
        return Err(MembershipError::InvalidIdentity("version"));
    }
    let identity = DeviceIdentity {
        device_id: DeviceId::from_bytes(cursor.array()?),
        ed25519_public_key: cursor.array()?,
        x25519_public_key: cursor.array()?,
        display_name: cursor.string()?,
        protocol_version: cursor.u16()?,
        self_signature: cursor.array()?,
    };
    identity.verify()?;
    Ok(identity)
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}
impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }
    fn take(&mut self, len: usize) -> Result<&'a [u8], MembershipError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or(MembershipError::Encoding)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(MembershipError::Encoding)?;
        self.offset = end;
        Ok(value)
    }
    fn byte(&mut self) -> Result<u8, MembershipError> {
        Ok(*self.take(1)?.first().ok_or(MembershipError::Encoding)?)
    }
    fn u16(&mut self) -> Result<u16, MembershipError> {
        Ok(u16::from_le_bytes(
            self.take(2)?
                .try_into()
                .map_err(|_| MembershipError::Encoding)?,
        ))
    }
    fn u32(&mut self) -> Result<u32, MembershipError> {
        Ok(u32::from_le_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| MembershipError::Encoding)?,
        ))
    }
    fn u64(&mut self) -> Result<u64, MembershipError> {
        Ok(u64::from_le_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| MembershipError::Encoding)?,
        ))
    }
    fn hlc(&mut self) -> Result<Hlc, MembershipError> {
        Ok(Hlc::new(self.u64()?, self.u32()?))
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], MembershipError> {
        self.take(N)?
            .try_into()
            .map_err(|_| MembershipError::Encoding)
    }
    fn string(&mut self) -> Result<String, MembershipError> {
        let len = usize::from(self.u16()?);
        if len > MAX_DEVICE_DISPLAY_NAME_BYTES {
            return Err(MembershipError::RecordTooLarge);
        }
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| MembershipError::Encoding)
    }
    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(key: &Ed25519Keypair, name: &str) -> DeviceIdentity {
        DeviceIdentity::new_signed(
            name,
            1,
            key,
            crate::X25519Keypair::from_private_bytes([7; 32]).public_key(),
        )
        .unwrap()
    }

    #[test]
    fn signed_records_round_trip_and_hash_is_signature_sensitive() {
        let key = Ed25519Keypair::from_private_bytes([1; 32]);
        let id = identity(&key, "local");
        let record =
            create_genesis(VaultId::from_bytes([2; 16]), id, Hlc::new(3, 4), &key).unwrap();
        let bytes = record.to_canonical_bytes().unwrap();
        assert_eq!(
            MembershipRecord::from_canonical_bytes(&bytes).unwrap(),
            record
        );
        let mut changed = bytes.clone();
        *changed.last_mut().unwrap() ^= 1;
        let changed_record = MembershipRecord::from_canonical_bytes(&changed).unwrap();
        assert_ne!(
            record.record_hash().unwrap(),
            changed_record.record_hash().unwrap()
        );
    }

    #[test]
    fn validation_is_input_order_independent_and_pending_is_known() {
        let root_key = Ed25519Keypair::from_private_bytes([1; 32]);
        let child_key = Ed25519Keypair::from_private_bytes([2; 32]);
        let root = identity(&root_key, "root");
        let child = identity(&child_key, "child");
        let vault = VaultId::from_bytes([3; 16]);
        let genesis = create_genesis(vault, root.clone(), Hlc::new(1, 0), &root_key).unwrap();
        let admission = create_admission(
            vault,
            child.clone(),
            root.device_id,
            Hlc::new(2, 0),
            &root_key,
        )
        .unwrap();
        let snapshot = validate_membership(vault, &[admission.clone(), genesis.clone()]).unwrap();
        assert_eq!(snapshot.active_member_count(), 1);
        assert!(snapshot.is_known(child.device_id));
    }

    #[test]
    fn blocking_is_local_monotonic_and_reversible() {
        let root_key = Ed25519Keypair::from_private_bytes([11; 32]);
        let child_key = Ed25519Keypair::from_private_bytes([12; 32]);
        let root = identity(&root_key, "root");
        let child = identity(&child_key, "child");
        let vault = VaultId::from_bytes([13; 16]);
        let genesis = create_genesis(vault, root.clone(), Hlc::new(1, 0), &root_key).unwrap();
        let admission = create_admission(
            vault,
            child.clone(),
            root.device_id,
            Hlc::new(2, 0),
            &root_key,
        )
        .unwrap();
        let acceptance = create_acceptance(
            vault,
            admission.record_hash().unwrap(),
            child.device_id,
            Hlc::new(3, 0),
            &child_key,
        )
        .unwrap();
        let mut db = Db::open_in_memory().unwrap();
        insert_membership_record(&mut db, &genesis).unwrap();
        insert_membership_record(&mut db, &admission).unwrap();
        insert_membership_record(&mut db, &acceptance).unwrap();
        block_device(&mut db, root.device_id, child.device_id, Hlc::new(10, 1)).unwrap();
        block_device(&mut db, root.device_id, child.device_id, Hlc::new(9, 9)).unwrap();
        let authorization = load_authorization(&db, vault).unwrap();
        assert!(authorization.is_locally_blocked(child.device_id));
        assert!(authorization.authorize_peer(child.device_id).is_err());
        assert!(unblock_device(&mut db, child.device_id).unwrap());
        assert!(
            !load_authorization(&db, vault)
                .unwrap()
                .is_locally_blocked(child.device_id)
        );
        assert!(!unblock_device(&mut db, child.device_id).unwrap());
        assert!(matches!(
            block_device(&mut db, root.device_id, root.device_id, Hlc::new(1, 0)),
            Err(MembershipError::CannotBlockLocalDevice)
        ));
    }
}
