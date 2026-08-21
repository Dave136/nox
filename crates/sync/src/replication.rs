//! Bounded cursor-map replication over one authenticated Noise connection.

use crate::noise::{NoiseConnection, NoiseError, preferred_initiator};
use locker_core::{
    AuthorizationSnapshot, BatchApplyResult, Change, DeviceId, MembershipRecord,
    MembershipRecordHash, ReplicationStore, ReplicationStoreError, VaultId, decode_change,
    encode_change,
};
pub use locker_core::{
    MAX_BATCH_PLAINTEXT_BYTES, MAX_CHANGE_CIPHERTEXT_BYTES, MAX_CHANGES_PER_BATCH,
    MAX_CURSOR_ENTRIES, MAX_ENCODED_CHANGE_BYTES,
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    time::{self, Instant},
};

pub const CHANGE_CHUNK_BYTES: usize = 48 * 1024;
pub const MAX_MEMBERSHIP_BATCH_BYTES: usize = 60 * 1024;
pub const MAX_MEMBERSHIP_HASHES_PER_MESSAGE: usize = 128;
pub const MAX_CONCURRENT_LARGE_REASSEMBLIES: usize = 2;
pub const MAX_ROUNDS_PER_SESSION: u32 = 1_024;
pub const SESSION_DEADLINE: Duration = Duration::from_secs(60);

const MESSAGE_MAGIC: &[u8; 4] = b"LRS1";
const MESSAGE_VERSION: u16 = 1;
const HEADER_BYTES: usize = 16;
const TAG_HELLO: u8 = 1;
const TAG_MEMBERSHIP_SUMMARY: u8 = 2;
const TAG_MEMBERSHIP_REQUEST: u8 = 3;
const TAG_MEMBERSHIP_RECORDS: u8 = 4;
const TAG_MEMBERSHIP_COMPLETE: u8 = 5;
const TAG_CURSOR_SUMMARY: u8 = 6;
const TAG_CHANGE_BATCH: u8 = 7;
const TAG_LARGE_START: u8 = 8;
const TAG_LARGE_CHUNK: u8 = 9;
const TAG_LARGE_END: u8 = 10;
const TAG_ACK: u8 = 11;
const TAG_ROUND_COMPLETE: u8 = 12;
const TAG_SESSION_COMPLETE: u8 = 13;
const TAG_ABORT: u8 = 14;

pub type CursorMap = locker_core::CursorMap;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CursorSummary {
    pub vault_id: VaultId,
    pub entries: CursorMap,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChangeBatch {
    pub batch_id: u64,
    pub encoded_changes: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplicationMessage {
    Hello {
        vault_id: VaultId,
        protocol_version: u16,
        sender_device_id: DeviceId,
    },
    MembershipSummary {
        set_hash: [u8; 32],
        record_count: u16,
    },
    /// Bounded hash stream: inventory first, then the computed missing set.
    MembershipRequest {
        missing_hashes: Vec<[u8; 32]>,
    },
    MembershipRecords {
        records: Vec<Vec<u8>>,
    },
    MembershipComplete {
        set_hash: [u8; 32],
    },
    CursorSummary {
        entries: CursorMap,
    },
    ChangeBatch {
        batch_id: u64,
        encoded_changes: Vec<Vec<u8>>,
    },
    LargeChangeStart {
        batch_id: u64,
        encoded_len: u32,
        sha256: [u8; 32],
    },
    LargeChangeChunk {
        batch_id: u64,
        chunk_index: u32,
        bytes: Vec<u8>,
    },
    LargeChangeEnd {
        batch_id: u64,
        chunk_count: u32,
    },
    Ack {
        batch_id: u64,
        committed_cursors: CursorMap,
    },
    RoundComplete {
        cursor_hash: [u8; 32],
    },
    SessionComplete {
        cursor_hash: [u8; 32],
    },
    Abort {
        public_reason: u8,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplicationCodecError {
    InvalidMessage,
    ReplayOrOrdering,
    MessageTooLarge,
    BatchTooLarge,
    ChangeTooLarge,
    InvalidCursor,
}

impl fmt::Display for ReplicationCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidMessage => "invalid replication message",
            Self::ReplayOrOrdering => "replication message ordering failure",
            Self::MessageTooLarge => "replication message is too large",
            Self::BatchTooLarge => "replication batch is too large",
            Self::ChangeTooLarge => "replication change is too large",
            Self::InvalidCursor => "invalid replication cursor",
        })
    }
}

impl std::error::Error for ReplicationCodecError {}

#[derive(Debug)]
pub enum ReplicationError {
    WrongVault,
    ProtocolMismatch,
    UnauthorizedPeer,
    InvalidMembership,
    InvalidCursor,
    InvalidMessage,
    ReplayOrOrdering,
    BatchTooLarge,
    ChangeTooLarge,
    InvalidAuthor,
    InvalidSignature,
    ConflictingDuplicate,
    InvalidAcknowledgement,
    DeadlineExceeded,
    RoundLimitExceeded,
    CapacityReached,
    NotConverged,
    Store(ReplicationStoreError),
    Transport(NoiseError),
    Codec(ReplicationCodecError),
    BlockingTask,
}

impl fmt::Display for ReplicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => error.fmt(formatter),
            Self::Transport(error) => error.fmt(formatter),
            Self::Codec(error) => error.fmt(formatter),
            other => formatter.write_str(match other {
                Self::WrongVault => "replication vault mismatch",
                Self::ProtocolMismatch => "replication protocol mismatch",
                Self::UnauthorizedPeer => "replication peer is not authorized",
                Self::InvalidMembership => "invalid replication membership",
                Self::InvalidCursor => "invalid replication cursor",
                Self::InvalidMessage => "invalid replication message",
                Self::ReplayOrOrdering => "replication message ordering failure",
                Self::BatchTooLarge => "replication batch is too large",
                Self::ChangeTooLarge => "replication change is too large",
                Self::InvalidAuthor => "invalid replication author",
                Self::InvalidSignature => "invalid replication signature",
                Self::ConflictingDuplicate => "conflicting replication duplicate",
                Self::InvalidAcknowledgement => "invalid replication acknowledgement",
                Self::DeadlineExceeded => "replication deadline exceeded",
                Self::RoundLimitExceeded => "replication round limit exceeded",
                Self::CapacityReached => "replication capacity reached",
                Self::NotConverged => "replication did not converge",
                Self::BlockingTask => "replication blocking task failed",
                Self::Store(_) | Self::Transport(_) | Self::Codec(_) => unreachable!(),
            }),
        }
    }
}

impl std::error::Error for ReplicationError {}

impl From<NoiseError> for ReplicationError {
    fn from(error: NoiseError) -> Self {
        Self::Transport(error)
    }
}

impl From<ReplicationStoreError> for ReplicationError {
    fn from(error: ReplicationStoreError) -> Self {
        match error {
            ReplicationStoreError::WrongVault => Self::WrongVault,
            ReplicationStoreError::UnknownOrigin => Self::InvalidAuthor,
            ReplicationStoreError::BlockedAuthor => Self::UnauthorizedPeer,
            ReplicationStoreError::Membership(_) => Self::InvalidMembership,
            ReplicationStoreError::InvalidCursor => Self::InvalidCursor,
            ReplicationStoreError::BatchTooLarge => Self::BatchTooLarge,
            ReplicationStoreError::ChangeTooLarge => Self::ChangeTooLarge,
            ReplicationStoreError::InvalidEncoding => Self::InvalidMessage,
            ReplicationStoreError::UnsupportedStoredChange => {
                Self::Store(ReplicationStoreError::UnsupportedStoredChange)
            }
            ReplicationStoreError::Journal(locker_core::JournalError::InvalidSignature) => {
                Self::InvalidSignature
            }
            ReplicationStoreError::Journal(locker_core::JournalError::ConflictingDuplicate) => {
                Self::ConflictingDuplicate
            }
            other => Self::Store(other),
        }
    }
}

impl From<ReplicationCodecError> for ReplicationError {
    fn from(error: ReplicationCodecError) -> Self {
        match error {
            ReplicationCodecError::ReplayOrOrdering => Self::ReplayOrOrdering,
            ReplicationCodecError::MessageTooLarge | ReplicationCodecError::BatchTooLarge => {
                Self::BatchTooLarge
            }
            ReplicationCodecError::ChangeTooLarge => Self::ChangeTooLarge,
            ReplicationCodecError::InvalidCursor => Self::InvalidCursor,
            ReplicationCodecError::InvalidMessage => Self::InvalidMessage,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ReplicationConfig {
    pub deadline: Duration,
    pub max_rounds: u32,
}

impl Default for ReplicationConfig {
    fn default() -> Self {
        Self {
            deadline: SESSION_DEADLINE,
            max_rounds: MAX_ROUNDS_PER_SESSION,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplicationReport {
    pub peer: DeviceId,
    pub rounds: u32,
    pub sent_changes: u64,
    pub received_changes: u64,
    pub final_cursors: CursorMap,
    pub converged: bool,
}

#[derive(Clone)]
pub struct ReplicationStoreHandle(Arc<Mutex<ReplicationStore>>);

impl fmt::Debug for ReplicationStoreHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ReplicationStoreHandle(<redacted>)")
    }
}

impl ReplicationStoreHandle {
    #[must_use]
    pub fn new(store: ReplicationStore) -> Self {
        Self(Arc::new(Mutex::new(store)))
    }

    async fn run<T, F>(&self, operation: F) -> Result<T, ReplicationError>
    where
        T: Send + 'static,
        F: FnOnce(&mut ReplicationStore) -> Result<T, ReplicationStoreError> + Send + 'static,
    {
        crate::spawn_core_blocking({
            let store = Arc::clone(&self.0);
            move || {
                let mut store = store.lock().map_err(|_| ReplicationError::BlockingTask)?;
                operation(&mut store).map_err(ReplicationError::from)
            }
        })
        .await
        .map_err(|_| ReplicationError::BlockingTask)?
    }

    pub async fn vault_id(&self) -> Result<VaultId, ReplicationError> {
        self.run(|store| Ok(store.vault_id())).await
    }

    pub async fn authorization_snapshot(&self) -> Result<AuthorizationSnapshot, ReplicationError> {
        self.run(|store| store.authorization_snapshot()).await
    }

    pub async fn membership_records(&self) -> Result<Vec<MembershipRecord>, ReplicationError> {
        self.run(|store| store.membership_records()).await
    }

    pub async fn membership_hash(&self) -> Result<[u8; 32], ReplicationError> {
        self.run(|store| store.membership_set_hash()).await
    }

    pub async fn merge_membership_records(
        &self,
        records: Vec<MembershipRecord>,
    ) -> Result<(), ReplicationError> {
        self.run(move |store| store.merge_membership_records(&records))
            .await
    }

    pub async fn cursor_map(&self) -> Result<CursorMap, ReplicationError> {
        self.run(|store| store.cursor_map()).await
    }

    pub async fn missing_changes(
        &self,
        remote: CursorMap,
    ) -> Result<Vec<Change>, ReplicationError> {
        self.run(move |store| {
            store.missing_changes(&remote, MAX_CHANGES_PER_BATCH, MAX_BATCH_PLAINTEXT_BYTES)
        })
        .await
    }

    pub async fn apply_remote_batch(
        &self,
        peer: DeviceId,
        authorization: AuthorizationSnapshot,
        changes: Vec<Change>,
    ) -> Result<BatchApplyResult, ReplicationError> {
        self.run(move |store| store.apply_remote_batch(peer, &authorization, &changes))
            .await
    }
}

#[derive(Default)]
struct MessageCodec {
    next_outbound: u64,
    last_inbound: u64,
}

impl MessageCodec {
    fn encode(&mut self, message: &ReplicationMessage) -> Result<(u64, Vec<u8>), ReplicationError> {
        self.next_outbound = self
            .next_outbound
            .checked_add(1)
            .ok_or(ReplicationError::ReplayOrOrdering)?;
        let id = self.next_outbound;
        Ok((id, encode_message(id, message)?))
    }

    fn decode(&mut self, bytes: &[u8]) -> Result<ReplicationMessage, ReplicationError> {
        let (id, message) = decode_message(bytes)?;
        if id <= self.last_inbound {
            return Err(ReplicationError::ReplayOrOrdering);
        }
        self.last_inbound = id;
        Ok(message)
    }
}

pub fn encode_message(
    message_id: u64,
    message: &ReplicationMessage,
) -> Result<Vec<u8>, ReplicationCodecError> {
    if message_id == 0 {
        return Err(ReplicationCodecError::ReplayOrOrdering);
    }
    let mut out = Vec::with_capacity(HEADER_BYTES + 128);
    out.extend_from_slice(MESSAGE_MAGIC);
    out.push(message_tag(message));
    out.push(0);
    out.extend_from_slice(&MESSAGE_VERSION.to_le_bytes());
    out.extend_from_slice(&message_id.to_le_bytes());
    encode_body(&mut out, message)?;
    if out.len() > crate::noise::MAX_NOISE_PLAINTEXT_BYTES {
        return Err(ReplicationCodecError::MessageTooLarge);
    }
    Ok(out)
}

pub fn decode_message(bytes: &[u8]) -> Result<(u64, ReplicationMessage), ReplicationCodecError> {
    if bytes.len() < HEADER_BYTES || bytes.len() > crate::noise::MAX_NOISE_PLAINTEXT_BYTES {
        return Err(ReplicationCodecError::MessageTooLarge);
    }
    let mut cursor = Cursor::new(bytes);
    if cursor.take(4)? != MESSAGE_MAGIC {
        return Err(ReplicationCodecError::InvalidMessage);
    }
    let tag = cursor.byte()?;
    if cursor.byte()? != 0 || cursor.u16()? != MESSAGE_VERSION {
        return Err(ReplicationCodecError::InvalidMessage);
    }
    let message_id = cursor.u64()?;
    if message_id == 0 {
        return Err(ReplicationCodecError::ReplayOrOrdering);
    }
    let message = decode_body(&mut cursor, tag)?;
    if !cursor.empty() {
        return Err(ReplicationCodecError::InvalidMessage);
    }
    Ok((message_id, message))
}

pub fn encode_replication_message(
    message_id: u64,
    message: &ReplicationMessage,
) -> Result<Vec<u8>, ReplicationCodecError> {
    encode_message(message_id, message)
}

pub fn decode_replication_message(
    bytes: &[u8],
) -> Result<(u64, ReplicationMessage), ReplicationCodecError> {
    decode_message(bytes)
}

fn message_tag(message: &ReplicationMessage) -> u8 {
    match message {
        ReplicationMessage::Hello { .. } => TAG_HELLO,
        ReplicationMessage::MembershipSummary { .. } => TAG_MEMBERSHIP_SUMMARY,
        ReplicationMessage::MembershipRequest { .. } => TAG_MEMBERSHIP_REQUEST,
        ReplicationMessage::MembershipRecords { .. } => TAG_MEMBERSHIP_RECORDS,
        ReplicationMessage::MembershipComplete { .. } => TAG_MEMBERSHIP_COMPLETE,
        ReplicationMessage::CursorSummary { .. } => TAG_CURSOR_SUMMARY,
        ReplicationMessage::ChangeBatch { .. } => TAG_CHANGE_BATCH,
        ReplicationMessage::LargeChangeStart { .. } => TAG_LARGE_START,
        ReplicationMessage::LargeChangeChunk { .. } => TAG_LARGE_CHUNK,
        ReplicationMessage::LargeChangeEnd { .. } => TAG_LARGE_END,
        ReplicationMessage::Ack { .. } => TAG_ACK,
        ReplicationMessage::RoundComplete { .. } => TAG_ROUND_COMPLETE,
        ReplicationMessage::SessionComplete { .. } => TAG_SESSION_COMPLETE,
        ReplicationMessage::Abort { .. } => TAG_ABORT,
    }
}

fn encode_body(
    out: &mut Vec<u8>,
    message: &ReplicationMessage,
) -> Result<(), ReplicationCodecError> {
    match message {
        ReplicationMessage::Hello {
            vault_id,
            protocol_version,
            sender_device_id,
        } => {
            out.extend_from_slice(vault_id.as_bytes());
            out.extend_from_slice(&protocol_version.to_le_bytes());
            out.extend_from_slice(sender_device_id.as_bytes());
        }
        ReplicationMessage::MembershipSummary {
            set_hash,
            record_count,
        } => {
            out.extend_from_slice(set_hash);
            out.extend_from_slice(&record_count.to_le_bytes());
        }
        ReplicationMessage::MembershipRequest { missing_hashes } => {
            if missing_hashes.len() > MAX_MEMBERSHIP_HASHES_PER_MESSAGE {
                return Err(ReplicationCodecError::BatchTooLarge);
            }
            put_u16(out, missing_hashes.len())?;
            for hash in missing_hashes {
                out.extend_from_slice(hash);
            }
        }
        ReplicationMessage::MembershipRecords { records } => encode_records(out, records)?,
        ReplicationMessage::MembershipComplete { set_hash }
        | ReplicationMessage::RoundComplete {
            cursor_hash: set_hash,
        }
        | ReplicationMessage::SessionComplete {
            cursor_hash: set_hash,
        } => {
            out.extend_from_slice(set_hash);
        }
        ReplicationMessage::CursorSummary { entries } => encode_cursors(out, entries)?,
        ReplicationMessage::ChangeBatch {
            batch_id,
            encoded_changes,
        } => {
            if *batch_id == 0
                || encoded_changes.is_empty()
                || encoded_changes.len() > MAX_CHANGES_PER_BATCH
            {
                return Err(ReplicationCodecError::BatchTooLarge);
            }
            put_u64(out, *batch_id);
            put_u16(out, encoded_changes.len())?;
            let mut total = 0usize;
            for change in encoded_changes {
                if change.len() > MAX_ENCODED_CHANGE_BYTES {
                    return Err(ReplicationCodecError::ChangeTooLarge);
                }
                total = total
                    .checked_add(change.len())
                    .ok_or(ReplicationCodecError::BatchTooLarge)?;
                if total > MAX_BATCH_PLAINTEXT_BYTES {
                    return Err(ReplicationCodecError::BatchTooLarge);
                }
                put_u32(out, change.len())?;
                out.extend_from_slice(change);
            }
        }
        ReplicationMessage::LargeChangeStart {
            batch_id,
            encoded_len,
            sha256,
        } => {
            if *batch_id == 0
                || *encoded_len == 0
                || usize::try_from(*encoded_len)
                    .map_err(|_| ReplicationCodecError::ChangeTooLarge)?
                    > MAX_ENCODED_CHANGE_BYTES
            {
                return Err(ReplicationCodecError::ChangeTooLarge);
            }
            put_u64(out, *batch_id);
            out.extend_from_slice(&encoded_len.to_le_bytes());
            out.extend_from_slice(sha256);
        }
        ReplicationMessage::LargeChangeChunk {
            batch_id,
            chunk_index,
            bytes,
        } => {
            if *batch_id == 0 || bytes.is_empty() || bytes.len() > CHANGE_CHUNK_BYTES {
                return Err(ReplicationCodecError::ChangeTooLarge);
            }
            put_u64(out, *batch_id);
            out.extend_from_slice(&chunk_index.to_le_bytes());
            put_u32(out, bytes.len())?;
            out.extend_from_slice(bytes);
        }
        ReplicationMessage::LargeChangeEnd {
            batch_id,
            chunk_count,
        } => {
            if *batch_id == 0 || *chunk_count == 0 {
                return Err(ReplicationCodecError::ChangeTooLarge);
            }
            put_u64(out, *batch_id);
            out.extend_from_slice(&chunk_count.to_le_bytes());
        }
        ReplicationMessage::Ack {
            batch_id,
            committed_cursors,
        } => {
            if *batch_id == 0 {
                return Err(ReplicationCodecError::InvalidMessage);
            }
            put_u64(out, *batch_id);
            encode_cursors(out, committed_cursors)?;
        }
        ReplicationMessage::Abort { public_reason } => out.push(*public_reason),
    }
    Ok(())
}

fn decode_body(
    cursor: &mut Cursor<'_>,
    tag: u8,
) -> Result<ReplicationMessage, ReplicationCodecError> {
    Ok(match tag {
        TAG_HELLO => ReplicationMessage::Hello {
            vault_id: VaultId::from_bytes(cursor.array()?),
            protocol_version: cursor.u16()?,
            sender_device_id: DeviceId::from_bytes(cursor.array()?),
        },
        TAG_MEMBERSHIP_SUMMARY => {
            let set_hash = cursor.array()?;
            let record_count = cursor.u16()?;
            if usize::from(record_count) > locker_core::MAX_MEMBERSHIP_RECORDS {
                return Err(ReplicationCodecError::BatchTooLarge);
            }
            ReplicationMessage::MembershipSummary {
                set_hash,
                record_count,
            }
        }
        TAG_MEMBERSHIP_REQUEST => {
            let count = usize::from(cursor.u16()?);
            if count > MAX_MEMBERSHIP_HASHES_PER_MESSAGE {
                return Err(ReplicationCodecError::BatchTooLarge);
            }
            ReplicationMessage::MembershipRequest {
                missing_hashes: (0..count)
                    .map(|_| cursor.array())
                    .collect::<Result<Vec<_>, _>>()?,
            }
        }
        TAG_MEMBERSHIP_RECORDS => ReplicationMessage::MembershipRecords {
            records: decode_records(cursor)?,
        },
        TAG_MEMBERSHIP_COMPLETE => ReplicationMessage::MembershipComplete {
            set_hash: cursor.array()?,
        },
        TAG_CURSOR_SUMMARY => ReplicationMessage::CursorSummary {
            entries: decode_cursors(cursor)?,
        },
        TAG_CHANGE_BATCH => {
            let batch_id = cursor.u64()?;
            let count = usize::from(cursor.u16()?);
            if batch_id == 0 || count == 0 || count > MAX_CHANGES_PER_BATCH {
                return Err(ReplicationCodecError::BatchTooLarge);
            }
            let mut total = 0usize;
            let mut encoded_changes = Vec::with_capacity(count);
            for _ in 0..count {
                let length = usize::try_from(cursor.u32()?)
                    .map_err(|_| ReplicationCodecError::ChangeTooLarge)?;
                if length > MAX_ENCODED_CHANGE_BYTES {
                    return Err(ReplicationCodecError::ChangeTooLarge);
                }
                total = total
                    .checked_add(length)
                    .ok_or(ReplicationCodecError::BatchTooLarge)?;
                if total > MAX_BATCH_PLAINTEXT_BYTES {
                    return Err(ReplicationCodecError::BatchTooLarge);
                }
                encoded_changes.push(cursor.take(length)?.to_vec());
            }
            ReplicationMessage::ChangeBatch {
                batch_id,
                encoded_changes,
            }
        }
        TAG_LARGE_START => {
            let batch_id = cursor.u64()?;
            let encoded_len = cursor.u32()?;
            if batch_id == 0
                || encoded_len == 0
                || usize::try_from(encoded_len)
                    .map_err(|_| ReplicationCodecError::ChangeTooLarge)?
                    > MAX_ENCODED_CHANGE_BYTES
            {
                return Err(ReplicationCodecError::ChangeTooLarge);
            }
            ReplicationMessage::LargeChangeStart {
                batch_id,
                encoded_len,
                sha256: cursor.array()?,
            }
        }
        TAG_LARGE_CHUNK => {
            let batch_id = cursor.u64()?;
            let chunk_index = cursor.u32()?;
            let length = usize::try_from(cursor.u32()?)
                .map_err(|_| ReplicationCodecError::ChangeTooLarge)?;
            if length == 0 || length > CHANGE_CHUNK_BYTES {
                return Err(ReplicationCodecError::ChangeTooLarge);
            }
            if batch_id == 0 {
                return Err(ReplicationCodecError::ChangeTooLarge);
            }
            ReplicationMessage::LargeChangeChunk {
                batch_id,
                chunk_index,
                bytes: cursor.take(length)?.to_vec(),
            }
        }
        TAG_LARGE_END => {
            let batch_id = cursor.u64()?;
            let chunk_count = cursor.u32()?;
            if batch_id == 0 || chunk_count == 0 {
                return Err(ReplicationCodecError::ChangeTooLarge);
            }
            ReplicationMessage::LargeChangeEnd {
                batch_id,
                chunk_count,
            }
        }
        TAG_ACK => {
            let batch_id = cursor.u64()?;
            if batch_id == 0 {
                return Err(ReplicationCodecError::InvalidMessage);
            }
            ReplicationMessage::Ack {
                batch_id,
                committed_cursors: decode_cursors(cursor)?,
            }
        }
        TAG_ROUND_COMPLETE => ReplicationMessage::RoundComplete {
            cursor_hash: cursor.array()?,
        },
        TAG_SESSION_COMPLETE => ReplicationMessage::SessionComplete {
            cursor_hash: cursor.array()?,
        },
        TAG_ABORT => ReplicationMessage::Abort {
            public_reason: cursor.byte()?,
        },
        _ => return Err(ReplicationCodecError::InvalidMessage),
    })
}

fn encode_records(out: &mut Vec<u8>, records: &[Vec<u8>]) -> Result<(), ReplicationCodecError> {
    if records.len() > locker_core::MAX_MEMBERSHIP_RECORDS {
        return Err(ReplicationCodecError::BatchTooLarge);
    }
    put_u16(out, records.len())?;
    let mut total = 0usize;
    for record in records {
        if record.len() > locker_core::MAX_MEMBERSHIP_RECORD_BYTES {
            return Err(ReplicationCodecError::BatchTooLarge);
        }
        total = total
            .checked_add(record.len())
            .ok_or(ReplicationCodecError::BatchTooLarge)?;
        if total > MAX_MEMBERSHIP_BATCH_BYTES {
            return Err(ReplicationCodecError::BatchTooLarge);
        }
        put_u32(out, record.len())?;
        out.extend_from_slice(record);
    }
    Ok(())
}

fn decode_records(cursor: &mut Cursor<'_>) -> Result<Vec<Vec<u8>>, ReplicationCodecError> {
    let count = usize::from(cursor.u16()?);
    if count > locker_core::MAX_MEMBERSHIP_RECORDS {
        return Err(ReplicationCodecError::BatchTooLarge);
    }
    let mut total = 0usize;
    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        let length =
            usize::try_from(cursor.u32()?).map_err(|_| ReplicationCodecError::BatchTooLarge)?;
        if length > locker_core::MAX_MEMBERSHIP_RECORD_BYTES {
            return Err(ReplicationCodecError::BatchTooLarge);
        }
        total = total
            .checked_add(length)
            .ok_or(ReplicationCodecError::BatchTooLarge)?;
        if total > MAX_MEMBERSHIP_BATCH_BYTES {
            return Err(ReplicationCodecError::BatchTooLarge);
        }
        records.push(cursor.take(length)?.to_vec());
    }
    Ok(records)
}

fn encode_cursors(out: &mut Vec<u8>, cursors: &CursorMap) -> Result<(), ReplicationCodecError> {
    if cursors.len() > MAX_CURSOR_ENTRIES {
        return Err(ReplicationCodecError::InvalidCursor);
    }
    let nonzero = cursors
        .iter()
        .filter(|(_, value)| **value != 0)
        .collect::<Vec<_>>();
    put_u16(out, nonzero.len())?;
    let mut previous = None;
    for (device, cursor) in nonzero {
        if *cursor > i64::MAX as u64 || previous.is_some_and(|old| old >= *device) {
            return Err(ReplicationCodecError::InvalidCursor);
        }
        previous = Some(*device);
        out.extend_from_slice(device.as_bytes());
        put_u64(out, *cursor);
    }
    Ok(())
}

fn decode_cursors(cursor: &mut Cursor<'_>) -> Result<CursorMap, ReplicationCodecError> {
    let count = usize::from(cursor.u16()?);
    if count > MAX_CURSOR_ENTRIES {
        return Err(ReplicationCodecError::InvalidCursor);
    }
    let mut output = BTreeMap::new();
    let mut previous = None;
    for _ in 0..count {
        let device = DeviceId::from_bytes(cursor.array()?);
        let value = cursor.u64()?;
        if value == 0 || value > i64::MAX as u64 || previous.is_some_and(|old| old >= device) {
            return Err(ReplicationCodecError::InvalidCursor);
        }
        previous = Some(device);
        output.insert(device, value);
    }
    Ok(output)
}

fn put_u16(out: &mut Vec<u8>, value: usize) -> Result<(), ReplicationCodecError> {
    out.extend_from_slice(
        &u16::try_from(value)
            .map_err(|_| ReplicationCodecError::MessageTooLarge)?
            .to_le_bytes(),
    );
    Ok(())
}

fn put_u32(out: &mut Vec<u8>, value: usize) -> Result<(), ReplicationCodecError> {
    out.extend_from_slice(
        &u32::try_from(value)
            .map_err(|_| ReplicationCodecError::MessageTooLarge)?
            .to_le_bytes(),
    );
    Ok(())
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], ReplicationCodecError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(ReplicationCodecError::InvalidMessage)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(ReplicationCodecError::InvalidMessage)?;
        self.offset = end;
        Ok(value)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], ReplicationCodecError> {
        self.take(N)?
            .try_into()
            .map_err(|_| ReplicationCodecError::InvalidMessage)
    }

    fn byte(&mut self) -> Result<u8, ReplicationCodecError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, ReplicationCodecError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32, ReplicationCodecError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, ReplicationCodecError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    fn empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

fn cursor_hash(cursors: &CursorMap) -> Result<[u8; 32], ReplicationError> {
    let mut bytes = Vec::with_capacity(cursors.len() * 40);
    encode_cursors(&mut bytes, cursors).map_err(ReplicationError::from)?;
    Ok(Sha256::digest(bytes).into())
}

fn membership_wire_map(
    records: &[MembershipRecord],
) -> Result<BTreeMap<MembershipRecordHash, Vec<u8>>, ReplicationError> {
    records.iter().try_fold(BTreeMap::new(), |mut map, record| {
        let bytes = record
            .to_canonical_bytes()
            .map_err(|_| ReplicationError::InvalidMembership)?;
        let hash = record
            .record_hash()
            .map_err(|_| ReplicationError::InvalidMembership)?;
        map.insert(hash, bytes);
        Ok(map)
    })
}

fn membership_batches(records: Vec<Vec<u8>>) -> Result<Vec<Vec<Vec<u8>>>, ReplicationError> {
    let mut batches = Vec::new();
    let mut batch = Vec::new();
    let mut raw_bytes = 0usize;
    let mut wire_bytes = HEADER_BYTES + 2;
    for record in records {
        if record.len() > locker_core::MAX_MEMBERSHIP_RECORD_BYTES {
            return Err(ReplicationError::BatchTooLarge);
        }
        let next_raw = raw_bytes
            .checked_add(record.len())
            .ok_or(ReplicationError::BatchTooLarge)?;
        let next_wire = wire_bytes
            .checked_add(4 + record.len())
            .ok_or(ReplicationError::BatchTooLarge)?;
        if !batch.is_empty()
            && (next_raw > MAX_MEMBERSHIP_BATCH_BYTES
                || next_wire > crate::noise::MAX_NOISE_PLAINTEXT_BYTES)
        {
            batches.push(std::mem::take(&mut batch));
            raw_bytes = 0;
            wire_bytes = HEADER_BYTES + 2;
        }
        if record.len() > MAX_MEMBERSHIP_BATCH_BYTES
            || HEADER_BYTES + 2 + 4 + record.len() > crate::noise::MAX_NOISE_PLAINTEXT_BYTES
        {
            return Err(ReplicationError::BatchTooLarge);
        }
        raw_bytes += record.len();
        wire_bytes += 4 + record.len();
        batch.push(record);
    }
    if !batch.is_empty() {
        batches.push(batch);
    }
    Ok(batches)
}

fn parse_membership_records(
    records: Vec<Vec<u8>>,
) -> Result<Vec<MembershipRecord>, ReplicationError> {
    records
        .into_iter()
        .map(|record| {
            MembershipRecord::from_canonical_bytes(&record)
                .map_err(|_| ReplicationError::InvalidMembership)
        })
        .collect()
}

async fn send_message<S>(
    connection: &mut NoiseConnection<S>,
    codec: &mut MessageCodec,
    message: &ReplicationMessage,
    deadline: Instant,
) -> Result<u64, ReplicationError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (id, bytes) = codec.encode(message)?;
    time::timeout_at(deadline, connection.send(&bytes))
        .await
        .map_err(|_| ReplicationError::DeadlineExceeded)??;
    Ok(id)
}

async fn receive_message<S>(
    connection: &mut NoiseConnection<S>,
    codec: &mut MessageCodec,
    deadline: Instant,
) -> Result<ReplicationMessage, ReplicationError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let bytes = time::timeout_at(deadline, connection.receive())
        .await
        .map_err(|_| ReplicationError::DeadlineExceeded)??;
    codec.decode(&bytes)
}

async fn exchange_messages<S>(
    connection: &mut NoiseConnection<S>,
    outbound: &mut MessageCodec,
    inbound: &mut MessageCodec,
    local_first: bool,
    message: &ReplicationMessage,
    deadline: Instant,
) -> Result<ReplicationMessage, ReplicationError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    if local_first {
        send_message(connection, outbound, message, deadline).await?;
        receive_message(connection, inbound, deadline).await
    } else {
        let remote = receive_message(connection, inbound, deadline).await?;
        send_message(connection, outbound, message, deadline).await?;
        Ok(remote)
    }
}

async fn exchange_hello<S>(
    connection: &mut NoiseConnection<S>,
    store: &ReplicationStoreHandle,
    outbound: &mut MessageCodec,
    inbound: &mut MessageCodec,
    local_first: bool,
    deadline: Instant,
) -> Result<(), ReplicationError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let hello = ReplicationMessage::Hello {
        vault_id: store.vault_id().await?,
        protocol_version: crate::frame::SYNC_PROTOCOL_VERSION,
        sender_device_id: connection.local_device_id(),
    };
    let remote =
        exchange_messages(connection, outbound, inbound, local_first, &hello, deadline).await?;
    match remote {
        ReplicationMessage::Hello {
            vault_id,
            protocol_version,
            sender_device_id,
        } => {
            if vault_id != hello_vault(&hello) {
                return Err(ReplicationError::WrongVault);
            }
            if protocol_version != crate::frame::SYNC_PROTOCOL_VERSION {
                return Err(ReplicationError::ProtocolMismatch);
            }
            if sender_device_id != connection.remote_device_id() {
                return Err(ReplicationError::UnauthorizedPeer);
            }
            Ok(())
        }
        ReplicationMessage::Abort { .. } => Err(ReplicationError::UnauthorizedPeer),
        _ => Err(ReplicationError::InvalidMessage),
    }
}

fn hello_vault(message: &ReplicationMessage) -> VaultId {
    match message {
        ReplicationMessage::Hello { vault_id, .. } => *vault_id,
        _ => unreachable!(),
    }
}

async fn exchange_membership<S>(
    connection: &mut NoiseConnection<S>,
    store: &ReplicationStoreHandle,
    outbound: &mut MessageCodec,
    inbound: &mut MessageCodec,
    local_first: bool,
    deadline: Instant,
) -> Result<(), ReplicationError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let local_hash = store.membership_hash().await?;
    let local_records = store.membership_records().await?;
    let summary = ReplicationMessage::MembershipSummary {
        set_hash: local_hash,
        record_count: u16::try_from(local_records.len())
            .map_err(|_| ReplicationError::InvalidMembership)?,
    };
    let remote_summary = exchange_messages(
        connection,
        outbound,
        inbound,
        local_first,
        &summary,
        deadline,
    )
    .await?;
    let ReplicationMessage::MembershipSummary {
        set_hash: remote_hash,
        record_count: remote_count,
    } = remote_summary
    else {
        return Err(ReplicationError::InvalidMessage);
    };
    if usize::from(remote_count) > locker_core::MAX_MEMBERSHIP_RECORDS {
        return Err(ReplicationError::InvalidMembership);
    }
    if remote_hash == local_hash && usize::from(remote_count) != local_records.len() {
        return Err(ReplicationError::InvalidMembership);
    }

    if remote_hash != local_hash {
        let local_wire = membership_wire_map(&local_records)?;
        let local_hashes = local_wire
            .keys()
            .map(|hash| *hash.as_bytes())
            .collect::<Vec<_>>();
        let remote_hashes = exchange_membership_hashes(
            connection,
            outbound,
            inbound,
            local_first,
            &local_hashes,
            usize::from(remote_count),
            deadline,
        )
        .await?;
        if remote_hashes.len() != usize::from(remote_count) {
            return Err(ReplicationError::InvalidMembership);
        }
        let remote_hashes = remote_hashes
            .into_iter()
            .map(MembershipRecordHash::from_bytes)
            .collect::<BTreeSet<_>>();
        if remote_hashes.len() != usize::from(remote_count) {
            return Err(ReplicationError::InvalidMembership);
        }
        let local_hashes = local_wire.keys().copied().collect::<BTreeSet<_>>();
        let requested = remote_hashes
            .difference(&local_hashes)
            .map(|hash| *hash.as_bytes())
            .collect::<Vec<_>>();
        let remote_requested = exchange_membership_requests(
            connection,
            outbound,
            inbound,
            local_first,
            &requested,
            local_hashes.difference(&remote_hashes).count(),
            deadline,
        )
        .await?
        .into_iter()
        .map(MembershipRecordHash::from_bytes)
        .collect::<Vec<_>>();
        let mut requested_set = BTreeSet::new();
        let mut outgoing = Vec::with_capacity(remote_requested.len());
        for hash in &remote_requested {
            if !requested_set.insert(*hash) {
                return Err(ReplicationError::InvalidMembership);
            }
            outgoing.push(
                local_wire
                    .get(hash)
                    .cloned()
                    .ok_or(ReplicationError::InvalidMembership)?,
            );
        }
        let received = exchange_membership_records(
            connection,
            outbound,
            inbound,
            local_first,
            membership_batches(outgoing)?,
            local_records.len(),
            usize::from(remote_count),
            requested,
            deadline,
        )
        .await?;
        if !received.is_empty() {
            store.merge_membership_records(received).await?;
        }
    }

    let hash = store.membership_hash().await?;
    let remote = exchange_messages(
        connection,
        outbound,
        inbound,
        local_first,
        &ReplicationMessage::MembershipComplete { set_hash: hash },
        deadline,
    )
    .await?;
    let ReplicationMessage::MembershipComplete { set_hash } = remote else {
        return Err(ReplicationError::InvalidMessage);
    };
    if set_hash != hash || store.membership_hash().await? != set_hash {
        return Err(ReplicationError::InvalidMembership);
    }
    store
        .authorization_snapshot()
        .await?
        .authorize_peer(connection.remote_device_id())
        .map_err(|_| ReplicationError::UnauthorizedPeer)?;
    Ok(())
}

async fn exchange_membership_hashes<S>(
    connection: &mut NoiseConnection<S>,
    outbound: &mut MessageCodec,
    inbound: &mut MessageCodec,
    local_first: bool,
    local_hashes: &[[u8; 32]],
    remote_count: usize,
    deadline: Instant,
) -> Result<Vec<[u8; 32]>, ReplicationError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut remote_hashes = Vec::new();
    let local_chunks = chunk_count(local_hashes.len());
    let remote_chunks = chunk_count(remote_count);
    let rounds = local_chunks.max(remote_chunks) + 1;
    for round in 0..rounds {
        let start = round * MAX_MEMBERSHIP_HASHES_PER_MESSAGE;
        let end = (start + MAX_MEMBERSHIP_HASHES_PER_MESSAGE).min(local_hashes.len());
        let message = ReplicationMessage::MembershipRequest {
            missing_hashes: if start < local_hashes.len() {
                local_hashes[start..end].to_vec()
            } else {
                Vec::new()
            },
        };
        let remote = exchange_messages(
            connection,
            outbound,
            inbound,
            local_first,
            &message,
            deadline,
        )
        .await?;
        let ReplicationMessage::MembershipRequest { missing_hashes } = remote else {
            return Err(ReplicationError::InvalidMessage);
        };
        if round < remote_chunks {
            if missing_hashes.is_empty() {
                return Err(ReplicationError::InvalidMembership);
            }
            remote_hashes.extend(missing_hashes);
        } else if !missing_hashes.is_empty() {
            return Err(ReplicationError::InvalidMembership);
        }
    }
    Ok(remote_hashes)
}

fn chunk_count(len: usize) -> usize {
    if len == 0 {
        0
    } else {
        (len - 1) / MAX_MEMBERSHIP_HASHES_PER_MESSAGE + 1
    }
}

async fn exchange_membership_requests<S>(
    connection: &mut NoiseConnection<S>,
    outbound: &mut MessageCodec,
    inbound: &mut MessageCodec,
    local_first: bool,
    local_missing: &[[u8; 32]],
    remote_missing_count: usize,
    deadline: Instant,
) -> Result<Vec<[u8; 32]>, ReplicationError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut remote_missing = Vec::new();
    let local_chunks = chunk_count(local_missing.len());
    let remote_chunks = chunk_count(remote_missing_count);
    let rounds = local_chunks.max(remote_chunks) + 1;
    for round in 0..rounds {
        let start = round * MAX_MEMBERSHIP_HASHES_PER_MESSAGE;
        let end = (start + MAX_MEMBERSHIP_HASHES_PER_MESSAGE).min(local_missing.len());
        let message = ReplicationMessage::MembershipRequest {
            missing_hashes: if start < local_missing.len() {
                local_missing[start..end].to_vec()
            } else {
                Vec::new()
            },
        };
        let remote = exchange_messages(
            connection,
            outbound,
            inbound,
            local_first,
            &message,
            deadline,
        )
        .await?;
        let ReplicationMessage::MembershipRequest { missing_hashes } = remote else {
            return Err(ReplicationError::InvalidMessage);
        };
        if round < remote_chunks {
            if missing_hashes.is_empty() {
                return Err(ReplicationError::InvalidMembership);
            }
            remote_missing.extend(missing_hashes);
        } else if !missing_hashes.is_empty() {
            return Err(ReplicationError::InvalidMembership);
        }
    }
    Ok(remote_missing)
}

#[allow(clippy::too_many_arguments)]
async fn exchange_membership_records<S>(
    connection: &mut NoiseConnection<S>,
    outbound: &mut MessageCodec,
    inbound: &mut MessageCodec,
    local_first: bool,
    local_batches: Vec<Vec<Vec<u8>>>,
    local_record_count: usize,
    remote_record_count: usize,
    requested_hashes: Vec<[u8; 32]>,
    deadline: Instant,
) -> Result<Vec<MembershipRecord>, ReplicationError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut remote_records = Vec::new();
    let requested = requested_hashes
        .into_iter()
        .map(MembershipRecordHash::from_bytes)
        .collect::<BTreeSet<_>>();
    let mut received = BTreeSet::new();
    let rounds = local_record_count.max(remote_record_count) + 1;
    for round in 0..rounds {
        let records = local_batches.get(round).cloned().unwrap_or_default();
        let remote = exchange_messages(
            connection,
            outbound,
            inbound,
            local_first,
            &ReplicationMessage::MembershipRecords { records },
            deadline,
        )
        .await?;
        let ReplicationMessage::MembershipRecords { records } = remote else {
            return Err(ReplicationError::InvalidMessage);
        };
        if records.is_empty() {
            continue;
        }
        for record in parse_membership_records(records)? {
            let hash = record
                .record_hash()
                .map_err(|_| ReplicationError::InvalidMembership)?;
            if !requested.contains(&hash) || !received.insert(hash) {
                return Err(ReplicationError::InvalidMembership);
            }
            remote_records.push(record);
        }
        if remote_records.len() > locker_core::MAX_MEMBERSHIP_RECORDS {
            return Err(ReplicationError::InvalidMembership);
        }
    }
    if received != requested {
        return Err(ReplicationError::InvalidMembership);
    }
    Ok(remote_records)
}

#[allow(clippy::too_many_arguments)]
async fn send_changes<S>(
    connection: &mut NoiseConnection<S>,
    store: &ReplicationStoreHandle,
    outbound: &mut MessageCodec,
    inbound: &mut MessageCodec,
    remote_cursors: &CursorMap,
    batch_id: u64,
    deadline: Instant,
) -> Result<(u64, CursorMap, bool), ReplicationError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let before = store.cursor_map().await?;
    let changes = store.missing_changes(remote_cursors.clone()).await?;
    if changes.is_empty() {
        send_message(
            connection,
            outbound,
            &ReplicationMessage::RoundComplete {
                cursor_hash: cursor_hash(&before)?,
            },
            deadline,
        )
        .await?;
        return Ok((0, before, false));
    }
    let encoded = changes
        .iter()
        .map(encode_change)
        .collect::<Result<Vec<_>, _>>()
        .map_err(ReplicationError::from)?;
    let total = encoded.iter().map(Vec::len).sum::<usize>();
    if encoded.len() > 1 || total <= MAX_BATCH_PLAINTEXT_BYTES {
        send_message(
            connection,
            outbound,
            &ReplicationMessage::ChangeBatch {
                batch_id,
                encoded_changes: encoded,
            },
            deadline,
        )
        .await?;
    } else {
        let bytes = encoded
            .into_iter()
            .next()
            .ok_or(ReplicationError::BatchTooLarge)?;
        let hash: [u8; 32] = Sha256::digest(&bytes).into();
        send_message(
            connection,
            outbound,
            &ReplicationMessage::LargeChangeStart {
                batch_id,
                encoded_len: u32::try_from(bytes.len())
                    .map_err(|_| ReplicationError::ChangeTooLarge)?,
                sha256: hash,
            },
            deadline,
        )
        .await?;
        let mut chunk_count = 0u32;
        for chunk in bytes.chunks(CHANGE_CHUNK_BYTES) {
            send_message(
                connection,
                outbound,
                &ReplicationMessage::LargeChangeChunk {
                    batch_id,
                    chunk_index: chunk_count,
                    bytes: chunk.to_vec(),
                },
                deadline,
            )
            .await?;
            chunk_count = chunk_count
                .checked_add(1)
                .ok_or(ReplicationError::ChangeTooLarge)?;
        }
        send_message(
            connection,
            outbound,
            &ReplicationMessage::LargeChangeEnd {
                batch_id,
                chunk_count,
            },
            deadline,
        )
        .await?;
    }
    let ack = receive_message(connection, inbound, deadline).await?;
    let ReplicationMessage::Ack {
        batch_id: acknowledged,
        committed_cursors,
    } = ack
    else {
        return Err(ReplicationError::InvalidAcknowledgement);
    };
    if acknowledged != batch_id {
        return Err(ReplicationError::InvalidAcknowledgement);
    }
    verify_ack(&committed_cursors, remote_cursors, &before, &changes)?;
    Ok((changes.len() as u64, committed_cursors, true))
}

fn verify_ack(
    ack: &CursorMap,
    baseline: &CursorMap,
    before: &CursorMap,
    sent_changes: &[Change],
) -> Result<(), ReplicationError> {
    if ack.len() > MAX_CURSOR_ENTRIES {
        return Err(ReplicationError::InvalidAcknowledgement);
    }
    let mut upper = before.clone();
    for change in sent_changes {
        upper
            .entry(change.origin_device_id)
            .and_modify(|value| *value = (*value).max(change.origin_seq))
            .or_insert(change.origin_seq);
    }
    for (origin, value) in baseline {
        upper.entry(*origin).or_insert(*value);
    }
    for (origin, value) in ack {
        if *value > i64::MAX as u64
            || *value < baseline.get(origin).copied().unwrap_or(0)
            || *value > upper.get(origin).copied().unwrap_or(0)
        {
            return Err(ReplicationError::InvalidAcknowledgement);
        }
        if !upper.contains_key(origin) {
            return Err(ReplicationError::InvalidAcknowledgement);
        }
    }
    Ok(())
}

async fn receive_changes<S>(
    connection: &mut NoiseConnection<S>,
    store: &ReplicationStoreHandle,
    outbound: &mut MessageCodec,
    inbound: &mut MessageCodec,
    peer: DeviceId,
    deadline: Instant,
) -> Result<(u64, bool), ReplicationError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let message = receive_message(connection, inbound, deadline).await?;
    match message {
        ReplicationMessage::RoundComplete {
            cursor_hash: remote_hash,
        } => {
            if remote_hash != cursor_hash(&store.cursor_map().await?)? {
                return Err(ReplicationError::InvalidCursor);
            }
            Ok((0, false))
        }
        ReplicationMessage::ChangeBatch {
            batch_id,
            encoded_changes,
        } => {
            let changes = encoded_changes
                .into_iter()
                .map(|bytes| decode_change(&bytes).map_err(ReplicationError::from))
                .collect::<Result<Vec<_>, _>>()?;
            let authorization = store.authorization_snapshot().await?;
            let result = store
                .apply_remote_batch(peer, authorization, changes)
                .await?;
            send_message(
                connection,
                outbound,
                &ReplicationMessage::Ack {
                    batch_id,
                    committed_cursors: result.cursors_after_commit,
                },
                deadline,
            )
            .await?;
            Ok((result.inserted as u64, true))
        }
        ReplicationMessage::LargeChangeStart {
            batch_id,
            encoded_len,
            sha256,
        } => {
            let permit = large_reassembly_permit()?;
            let encoded_len =
                usize::try_from(encoded_len).map_err(|_| ReplicationError::ChangeTooLarge)?;
            if encoded_len == 0 || encoded_len > MAX_ENCODED_CHANGE_BYTES {
                return Err(ReplicationError::ChangeTooLarge);
            }
            let mut buffer = Vec::with_capacity(encoded_len);
            let mut expected_index = 0u32;
            let chunk_count = loop {
                match receive_message(connection, inbound, deadline).await? {
                    ReplicationMessage::LargeChangeChunk {
                        batch_id: current,
                        chunk_index,
                        bytes,
                    } if current == batch_id && chunk_index == expected_index => {
                        if buffer
                            .len()
                            .checked_add(bytes.len())
                            .is_none_or(|len| len > encoded_len)
                        {
                            return Err(ReplicationError::ChangeTooLarge);
                        }
                        buffer.extend_from_slice(&bytes);
                        expected_index = expected_index
                            .checked_add(1)
                            .ok_or(ReplicationError::ChangeTooLarge)?;
                    }
                    ReplicationMessage::LargeChangeEnd {
                        batch_id: current,
                        chunk_count,
                    } if current == batch_id && chunk_count == expected_index => break chunk_count,
                    _ => return Err(ReplicationError::InvalidMessage),
                }
            };
            let _permit = permit;
            if buffer.len() != encoded_len || Sha256::digest(&buffer).as_slice() != sha256 {
                return Err(ReplicationError::ChangeTooLarge);
            }
            if chunk_count == 0 {
                return Err(ReplicationError::ChangeTooLarge);
            }
            let change = decode_change(&buffer).map_err(ReplicationError::from)?;
            let authorization = store.authorization_snapshot().await?;
            let result = store
                .apply_remote_batch(peer, authorization, vec![change])
                .await?;
            send_message(
                connection,
                outbound,
                &ReplicationMessage::Ack {
                    batch_id,
                    committed_cursors: result.cursors_after_commit,
                },
                deadline,
            )
            .await?;
            Ok((result.inserted as u64, true))
        }
        ReplicationMessage::Abort { .. } => Err(ReplicationError::UnauthorizedPeer),
        _ => Err(ReplicationError::InvalidMessage),
    }
}

fn large_reassembly_permit() -> Result<OwnedSemaphorePermit, ReplicationError> {
    static PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    PERMITS
        .get_or_init(|| Arc::new(Semaphore::new(MAX_CONCURRENT_LARGE_REASSEMBLIES)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| ReplicationError::CapacityReached)
}

pub async fn replicate<S>(
    connection: &mut NoiseConnection<S>,
    store: ReplicationStoreHandle,
    config: ReplicationConfig,
) -> Result<ReplicationReport, ReplicationError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    if config.deadline.is_zero()
        || config.max_rounds == 0
        || config.max_rounds > MAX_ROUNDS_PER_SESSION
    {
        return Err(ReplicationError::RoundLimitExceeded);
    }
    let deadline = Instant::now() + config.deadline;
    let local = connection.local_device_id();
    let peer = connection.remote_device_id();
    let preferred = preferred_initiator(local, peer).map_err(ReplicationError::Transport)?;
    let local_first = preferred == local;
    let mut outbound = MessageCodec::default();
    let mut inbound = MessageCodec::default();

    exchange_hello(
        connection,
        &store,
        &mut outbound,
        &mut inbound,
        local_first,
        deadline,
    )
    .await?;
    exchange_membership(
        connection,
        &store,
        &mut outbound,
        &mut inbound,
        local_first,
        deadline,
    )
    .await?;

    let mut sent_changes = 0u64;
    let mut received_changes = 0u64;
    for round in 1..=config.max_rounds {
        let local_cursors = store.cursor_map().await?;
        let summary = ReplicationMessage::CursorSummary {
            entries: local_cursors.clone(),
        };
        let remote = exchange_messages(
            connection,
            &mut outbound,
            &mut inbound,
            local_first,
            &summary,
            deadline,
        )
        .await?;
        let ReplicationMessage::CursorSummary { entries } = remote else {
            return Err(ReplicationError::InvalidMessage);
        };
        let (round_sent, local_had_data, round_received, remote_had_data) = if local_first {
            let (sent, _, had_data) = send_changes(
                connection,
                &store,
                &mut outbound,
                &mut inbound,
                &entries,
                round as u64,
                deadline,
            )
            .await?;
            let (received, had_remote) = receive_changes(
                connection,
                &store,
                &mut outbound,
                &mut inbound,
                peer,
                deadline,
            )
            .await?;
            (sent, had_data, received, had_remote)
        } else {
            let (received, had_remote) = receive_changes(
                connection,
                &store,
                &mut outbound,
                &mut inbound,
                peer,
                deadline,
            )
            .await?;
            let (sent, _, had_data) = send_changes(
                connection,
                &store,
                &mut outbound,
                &mut inbound,
                &entries,
                round as u64,
                deadline,
            )
            .await?;
            (sent, had_data, received, had_remote)
        };
        sent_changes = sent_changes.saturating_add(round_sent);
        received_changes = received_changes.saturating_add(round_received);
        let final_cursors = store.cursor_map().await?;
        if final_cursors == entries && !remote_had_data && !local_had_data {
            let hash = cursor_hash(&final_cursors)?;
            let complete = ReplicationMessage::SessionComplete { cursor_hash: hash };
            let peer_complete = exchange_messages(
                connection,
                &mut outbound,
                &mut inbound,
                local_first,
                &complete,
                deadline,
            )
            .await?;
            if peer_complete != complete {
                return Err(ReplicationError::NotConverged);
            }
            return Ok(ReplicationReport {
                peer,
                rounds: round,
                sent_changes,
                received_changes,
                final_cursors,
                converged: true,
            });
        }
    }
    Err(ReplicationError::RoundLimitExceeded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn membership_inventory_and_batches_are_bounded() {
        let hashes = vec![[7; 32]; MAX_MEMBERSHIP_HASHES_PER_MESSAGE];
        let message = ReplicationMessage::MembershipRequest {
            missing_hashes: hashes.clone(),
        };
        assert_eq!(
            decode_message(&encode_message(1, &message).unwrap()).unwrap(),
            (1, message)
        );
        assert_eq!(
            encode_message(
                1,
                &ReplicationMessage::MembershipRequest {
                    missing_hashes: vec![[7; 32]; MAX_MEMBERSHIP_HASHES_PER_MESSAGE + 1],
                },
            ),
            Err(ReplicationCodecError::BatchTooLarge)
        );

        let batches = membership_batches(vec![vec![3; 4 * 1024]; 40]).unwrap();
        assert!(batches.len() > 1);
        for (index, records) in batches.into_iter().enumerate() {
            let encoded = encode_message(
                index as u64 + 1,
                &ReplicationMessage::MembershipRecords { records },
            )
            .unwrap();
            assert!(encoded.len() <= crate::noise::MAX_NOISE_PLAINTEXT_BYTES);
        }
        assert!(matches!(
            membership_batches(vec![vec![0; locker_core::MAX_MEMBERSHIP_RECORD_BYTES + 1]]),
            Err(ReplicationError::BatchTooLarge)
        ));
    }

    #[test]
    fn membership_request_empty_batch_is_the_stream_sentinel() {
        let message = ReplicationMessage::MembershipRequest {
            missing_hashes: Vec::new(),
        };
        assert_eq!(
            decode_message(&encode_message(1, &message).unwrap()).unwrap(),
            (1, message)
        );
    }

    #[test]
    fn replication_limits_and_reassembly_capacity_fail_closed() {
        let first = large_reassembly_permit().unwrap();
        let second = large_reassembly_permit().unwrap();
        assert!(matches!(
            large_reassembly_permit(),
            Err(ReplicationError::CapacityReached)
        ));
        drop((first, second));
    }

    #[test]
    fn message_golden_header_and_cursor_round_trip() {
        let message = ReplicationMessage::CursorSummary {
            entries: BTreeMap::from([
                (DeviceId::from_bytes([2; 32]), 3),
                (DeviceId::from_bytes([4; 32]), 9),
            ]),
        };
        let encoded = encode_message(1, &message).unwrap();
        assert_eq!(&encoded[..4], b"LRS1");
        assert_eq!(encoded[4], TAG_CURSOR_SUMMARY);
        assert_eq!(
            u16::from_le_bytes([encoded[6], encoded[7]]),
            MESSAGE_VERSION
        );
        assert_eq!(decode_message(&encoded).unwrap(), (1, message));
    }

    #[test]
    fn malformed_ordered_cursor_and_large_boundaries_fail_closed() {
        let cursor = ReplicationMessage::CursorSummary {
            entries: BTreeMap::from([(DeviceId::from_bytes([2; 32]), i64::MAX as u64 + 1)]),
        };
        assert_eq!(
            encode_message(1, &cursor),
            Err(ReplicationCodecError::InvalidCursor)
        );

        let chunk = ReplicationMessage::LargeChangeChunk {
            batch_id: 2,
            chunk_index: 0,
            bytes: vec![7; CHANGE_CHUNK_BYTES],
        };
        assert!(decode_message(&encode_message(1, &chunk).unwrap()).is_ok());
        let oversized = ReplicationMessage::LargeChangeChunk {
            batch_id: 2,
            chunk_index: 0,
            bytes: vec![7; CHANGE_CHUNK_BYTES + 1],
        };
        assert_eq!(
            encode_message(1, &oversized),
            Err(ReplicationCodecError::ChangeTooLarge)
        );
    }
}
