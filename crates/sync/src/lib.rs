//! LAN discovery, pairing, and journal replication for Locker.

use std::sync::{Arc, OnceLock};
use tokio::sync::Semaphore;

static CORE_BLOCKING_PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();

pub(crate) async fn spawn_core_blocking<F, T>(job: F) -> Result<T, tokio::task::JoinError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let semaphore = Arc::clone(
        CORE_BLOCKING_PERMITS
            .get_or_init(|| Arc::new(Semaphore::new(session::MAX_BLOCKING_CORE_JOBS))),
    );
    let permit = semaphore
        .acquire_owned()
        .await
        .expect("core blocking semaphore is never closed");
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        job()
    })
    .await
}

pub mod discovery;
pub mod frame;
pub mod noise;
pub mod pairing;
pub mod replication;
pub mod session;
pub mod transport;

pub use discovery::{
    Advertisement, AdvertisementHandle, DiscoveredEndpoint, DiscoveryError, DiscoveryEvent,
    DiscoveryHealth, DiscoveryHealthStream, DiscoveryKey, DiscoveryService, DiscoveryStream,
    MAX_DISCOVERED_ENDPOINTS, MAX_TXT_VALUE_BYTES, SERVICE_TYPE, encode_txt,
    parse_resolved_service,
};
pub use frame::{
    FRAME_HEADER_BYTES, FRAME_PREFIX_BYTES, Frame, FrameClass, FrameError, FrameOperation,
    MAX_FRAME_BODY_BYTES, MAX_FRAME_PAYLOAD_BYTES, SYNC_PROTOCOL_VERSION, validate_frame_header,
};
pub use noise::{
    DuplicateDecision, MAX_NOISE_MESSAGE_BYTES, MAX_NOISE_PLAINTEXT_BYTES, NOISE_PATTERN,
    NOISE_TAG_BYTES, NoiseConfig, NoiseConnection, NoiseError, NoiseRole, SessionDirection,
    encode_noise_prologue, handshake, handshake_responder_candidates, preferred_initiator,
    resolve_duplicate,
};
pub use pairing::{
    CONFIRMATION_TAG_BYTES, JoinRequest, MAX_FAILED_ATTEMPTS, MAX_PAIRING_PLAINTEXT_BYTES,
    PAIRING_AEAD_TAG_BYTES, PAIRING_INSTANCE_BYTES, PAIRING_LIFETIME, PAIRING_LOCATOR_SYMBOLS,
    PAIRING_SECRET_SYMBOLS, PairingCodeDisplay, PairingCodeInput, PairingError, PairingInstanceId,
    PairingOffer, PairingOutcome, PairingSecret, PairingStoreHandle, PairingTarget, cancel_offer,
    create_offer, locator_for,
};
pub use replication::{
    CHANGE_CHUNK_BYTES, ChangeBatch, CursorMap, CursorSummary, MAX_BATCH_PLAINTEXT_BYTES,
    MAX_CHANGE_CIPHERTEXT_BYTES, MAX_CHANGES_PER_BATCH, MAX_CONCURRENT_LARGE_REASSEMBLIES,
    MAX_CURSOR_ENTRIES, MAX_ENCODED_CHANGE_BYTES, MAX_MEMBERSHIP_BATCH_BYTES,
    MAX_MEMBERSHIP_HASHES_PER_MESSAGE, MAX_ROUNDS_PER_SESSION, ReplicationCodecError,
    ReplicationConfig, ReplicationError, ReplicationMessage, ReplicationReport,
    ReplicationStoreHandle, SESSION_DEADLINE, decode_message, decode_replication_message,
    encode_message, encode_replication_message, replicate,
};
pub use session::{
    ErrorScope, JoinedVault, MAX_BLOCKING_CORE_JOBS, PairingInvitation, PairingPublicState,
    PeerState, SyncConfig, SyncError, SyncErrorCode, SyncEvent, SyncEventReceiver, SyncHandle,
    SyncOperation, SyncRequest, SyncService, SyncSummary,
};
pub use transport::{
    AcceptedConnection, CONNECT_TIMEOUT, ConnectionLimiter, ConnectionPermit, FramedIo,
    HANDSHAKE_TIMEOUT, IDLE_TIMEOUT, MAX_CONCURRENT_CONNECTIONS, READ_TIMEOUT, TransportError,
    TransportLimits, WRITE_TIMEOUT, accept, connect,
};
