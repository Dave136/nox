//! Mutually authenticated Noise KK sessions over the bounded Task B framing.

use crate::{
    frame::{Frame, FrameClass, FrameError, MAX_FRAME_PAYLOAD_BYTES, SYNC_PROTOCOL_VERSION},
    transport::FramedIo,
};
use locker_core::{DeviceId, SecretKey, VaultId, X25519Keypair};
use snow::error::{Error as SnowError, StateProblem};
use std::{fmt, sync::OnceLock};
use tokio::time::{self, Instant};

/// The fixed Noise suite used by Locker v1.
pub const NOISE_PATTERN: &str = "Noise_KK_25519_ChaChaPoly_BLAKE2s";
/// ChaChaPoly's authentication tag size.
pub const NOISE_TAG_BYTES: usize = 16;
/// The maximum ciphertext carried by one outer frame.
pub const MAX_NOISE_MESSAGE_BYTES: usize = MAX_FRAME_PAYLOAD_BYTES;
/// The largest plaintext that fits in one Noise transport message.
pub const MAX_NOISE_PLAINTEXT_BYTES: usize = MAX_NOISE_MESSAGE_BYTES - NOISE_TAG_BYTES;

const PROLOGUE_PREFIX: &[u8; 13] = b"LOCKER-NOISE\0";

/// Which side of a Noise handshake this connection drives.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NoiseRole {
    Initiator,
    Responder,
}

/// Inputs needed to build one authenticated connection.
pub struct NoiseConfig<'a> {
    pub role: NoiseRole,
    pub protocol_version: u16,
    pub vault_id: VaultId,
    pub local_device_id: DeviceId,
    pub remote_device_id: DeviceId,
    pub local_static_keypair: &'a X25519Keypair,
    pub remote_static_public_key: [u8; 32],
}

impl fmt::Debug for NoiseConfig<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NoiseConfig")
            .field("role", &self.role)
            .field("protocol_version", &self.protocol_version)
            .field("vault_id", &self.vault_id)
            .field("local_device_id", &self.local_device_id)
            .field("remote_device_id", &self.remote_device_id)
            .field("local_static_keypair", &"<redacted>")
            .field("remote_static_public_key", &"<redacted>")
            .finish()
    }
}

/// The established connection's direction from the local device's perspective.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionDirection {
    LocallyInitiated,
    RemotelyInitiated,
}

/// The deterministic result of duplicate-session arbitration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DuplicateDecision {
    KeepCandidate,
    KeepExisting,
}

/// Stable, redacted errors exposed by the Noise layer.
#[derive(Debug)]
pub enum NoiseError {
    InvalidConfiguration(&'static str),
    SameDevice,
    UnsupportedProtocol,
    HandshakeTimeout,
    HandshakeFailed,
    UnexpectedFrame,
    RemoteStaticMismatch,
    PlaintextTooLarge,
    EncryptFailed,
    DecryptFailed,
    NonceExhausted,
    Closed,
    Frame(FrameError),
}

impl fmt::Display for NoiseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfiguration(reason) => {
                write!(formatter, "invalid Noise configuration: {reason}")
            }
            Self::SameDevice => formatter.write_str("Noise peers must have distinct device IDs"),
            Self::UnsupportedProtocol => formatter.write_str("unsupported sync protocol version"),
            Self::HandshakeTimeout => formatter.write_str("Noise handshake timed out"),
            Self::HandshakeFailed => formatter.write_str("Noise handshake failed"),
            Self::UnexpectedFrame => formatter.write_str("unexpected Noise frame"),
            Self::RemoteStaticMismatch => formatter.write_str("Noise remote static key mismatch"),
            Self::PlaintextTooLarge => formatter.write_str("Noise plaintext is too large"),
            Self::EncryptFailed => formatter.write_str("Noise encryption failed"),
            Self::DecryptFailed => formatter.write_str("Noise decryption failed"),
            Self::NonceExhausted => formatter.write_str("Noise nonce exhausted"),
            Self::Closed => formatter.write_str("Noise connection is closed"),
            Self::Frame(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for NoiseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Frame(error) => Some(error),
            _ => None,
        }
    }
}

impl From<FrameError> for NoiseError {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

/// A completed, single-owner Noise transport connection.
pub struct NoiseConnection<S> {
    io: FramedIo<S>,
    transport: snow::TransportState,
    local_device_id: DeviceId,
    remote_device_id: DeviceId,
    role: NoiseRole,
    terminal: bool,
}

impl<S> fmt::Debug for NoiseConnection<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NoiseConnection")
            .field("local_device_id", &self.local_device_id)
            .field("remote_device_id", &self.remote_device_id)
            .field("role", &self.role)
            .field("terminal", &self.terminal)
            .finish_non_exhaustive()
    }
}

/// Encode the sole prologue format used by the Noise handshake.
#[must_use]
pub fn encode_noise_prologue(protocol_version: u16, vault_id: VaultId) -> [u8; 31] {
    let mut output = [0_u8; 31];
    output[..PROLOGUE_PREFIX.len()].copy_from_slice(PROLOGUE_PREFIX);
    output[PROLOGUE_PREFIX.len()..PROLOGUE_PREFIX.len() + 2]
        .copy_from_slice(&protocol_version.to_le_bytes());
    output[PROLOGUE_PREFIX.len() + 2..].copy_from_slice(vault_id.as_bytes());
    output
}

/// Return the device that must initiate a connection between two peers.
pub fn preferred_initiator(left: DeviceId, right: DeviceId) -> Result<DeviceId, NoiseError> {
    if left == right {
        return Err(NoiseError::SameDevice);
    }
    Ok(left.min(right))
}

/// Decide which of two established sessions survives duplicate arbitration.
pub fn resolve_duplicate(
    local: DeviceId,
    remote: DeviceId,
    existing: SessionDirection,
    candidate: SessionDirection,
) -> Result<DuplicateDecision, NoiseError> {
    let preferred = preferred_initiator(local, remote)?;
    let preferred_direction = if preferred == local {
        SessionDirection::LocallyInitiated
    } else {
        SessionDirection::RemotelyInitiated
    };

    if existing == preferred_direction {
        Ok(DuplicateDecision::KeepExisting)
    } else if candidate == preferred_direction {
        Ok(DuplicateDecision::KeepCandidate)
    } else {
        Ok(DuplicateDecision::KeepExisting)
    }
}

/// Complete the two-message KK handshake before returning a usable transport.
pub async fn handshake<S>(
    io: FramedIo<S>,
    config: NoiseConfig<'_>,
    deadline: Instant,
) -> Result<NoiseConnection<S>, NoiseError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    validate_config(&config)?;
    match time::timeout_at(deadline, handshake_inner(io, config)).await {
        Ok(result) => result,
        Err(_) => Err(NoiseError::HandshakeTimeout),
    }
}

/// Complete a responder handshake when the peer address is not yet known.
/// The first Noise frame is classified by the actor and then tried against
/// the bounded authorized-key set.  A wrong candidate is rejected before the
/// responder writes its second handshake message, so the same frame can be
/// checked against the next candidate without reopening the socket.
#[allow(clippy::too_many_arguments)]
pub async fn handshake_responder_candidates<S>(
    io: FramedIo<S>,
    first_frame: Frame,
    protocol_version: u16,
    vault_id: VaultId,
    local_device_id: DeviceId,
    local_static_keypair: &X25519Keypair,
    candidates: Vec<(DeviceId, [u8; 32])>,
    deadline: Instant,
) -> Result<(NoiseConnection<S>, DeviceId), NoiseError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    if first_frame.class != FrameClass::NoiseHandshake {
        return Err(NoiseError::UnexpectedFrame);
    }
    validate_candidate_config(
        protocol_version,
        local_device_id,
        local_static_keypair,
        &candidates,
    )?;
    match time::timeout_at(
        deadline,
        responder_candidates_inner(
            io,
            first_frame.payload,
            protocol_version,
            vault_id,
            local_device_id,
            local_static_keypair,
            candidates,
        ),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(NoiseError::HandshakeTimeout),
    }
}

fn validate_candidate_config(
    protocol_version: u16,
    local_device_id: DeviceId,
    local_static_keypair: &X25519Keypair,
    candidates: &[(DeviceId, [u8; 32])],
) -> Result<(), NoiseError> {
    if protocol_version != SYNC_PROTOCOL_VERSION {
        return Err(NoiseError::UnsupportedProtocol);
    }
    if candidates.is_empty() {
        return Err(NoiseError::RemoteStaticMismatch);
    }
    if local_static_keypair
        .private_key_bytes()
        .iter()
        .all(|byte| *byte == 0)
        || local_static_keypair
            .public_key()
            .iter()
            .all(|byte| *byte == 0)
        || candidates.iter().any(|(device_id, key)| {
            *device_id == local_device_id || key.iter().all(|byte| *byte == 0)
        })
    {
        return Err(NoiseError::InvalidConfiguration(
            "invalid static key candidate",
        ));
    }
    Ok(())
}

async fn responder_candidates_inner<S>(
    mut io: FramedIo<S>,
    first_payload: Vec<u8>,
    protocol_version: u16,
    vault_id: VaultId,
    local_device_id: DeviceId,
    local_static_keypair: &X25519Keypair,
    candidates: Vec<(DeviceId, [u8; 32])>,
) -> Result<(NoiseConnection<S>, DeviceId), NoiseError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let prologue = encode_noise_prologue(protocol_version, vault_id);
    for (remote_device_id, remote_static_public_key) in candidates {
        let local_private = SecretKey::from_bytes(local_static_keypair.private_key_bytes());
        let builder = snow::Builder::new(noise_params())
            .prologue(&prologue)
            .map_err(|_| NoiseError::HandshakeFailed)?
            .local_private_key(local_private.as_bytes())
            .map_err(|_| NoiseError::HandshakeFailed)?
            .remote_public_key(&remote_static_public_key)
            .map_err(|_| NoiseError::HandshakeFailed)?;
        let mut state = builder
            .build_responder()
            .map_err(|_| NoiseError::HandshakeFailed)?;
        drop(local_private);

        let mut message = vec![0_u8; MAX_NOISE_MESSAGE_BYTES];
        let mut payload = vec![0_u8; MAX_NOISE_MESSAGE_BYTES];
        let read = match state.read_message(&first_payload, &mut payload) {
            Ok(read) => read,
            Err(_) => continue,
        };
        if read != 0 || state.get_remote_static() != Some(remote_static_public_key.as_slice()) {
            continue;
        }
        let written = state
            .write_message(&[], &mut message)
            .map_err(|_| NoiseError::HandshakeFailed)?;
        io.write_frame(FrameClass::NoiseHandshake, &message[..written])
            .await
            .map_err(NoiseError::Frame)?;
        if !state.is_handshake_finished() {
            return Err(NoiseError::HandshakeFailed);
        }
        let transport = state
            .into_transport_mode()
            .map_err(|_| NoiseError::HandshakeFailed)?;
        return Ok((
            NoiseConnection {
                io,
                transport,
                local_device_id,
                remote_device_id,
                role: NoiseRole::Responder,
                terminal: false,
            },
            remote_device_id,
        ));
    }
    Err(NoiseError::RemoteStaticMismatch)
}

async fn handshake_inner<S>(
    mut io: FramedIo<S>,
    config: NoiseConfig<'_>,
) -> Result<NoiseConnection<S>, NoiseError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let prologue = encode_noise_prologue(config.protocol_version, config.vault_id);
    let local_private = SecretKey::from_bytes(config.local_static_keypair.private_key_bytes());
    let builder = snow::Builder::new(noise_params())
        .prologue(&prologue)
        .map_err(|_| NoiseError::HandshakeFailed)?
        .local_private_key(local_private.as_bytes())
        .map_err(|_| NoiseError::HandshakeFailed)?
        .remote_public_key(&config.remote_static_public_key)
        .map_err(|_| NoiseError::HandshakeFailed)?;

    let mut state = match config.role {
        NoiseRole::Initiator => builder.build_initiator(),
        NoiseRole::Responder => builder.build_responder(),
    }
    .map_err(|_| NoiseError::HandshakeFailed)?;
    drop(local_private);

    let mut message = vec![0_u8; MAX_NOISE_MESSAGE_BYTES];
    let mut payload = vec![0_u8; MAX_NOISE_MESSAGE_BYTES];

    match config.role {
        NoiseRole::Initiator => {
            let written = state
                .write_message(&[], &mut message)
                .map_err(|_| NoiseError::HandshakeFailed)?;
            io.write_frame(FrameClass::NoiseHandshake, &message[..written])
                .await
                .map_err(NoiseError::Frame)?;

            let frame = io.read_frame().await.map_err(NoiseError::Frame)?;
            if frame.class != FrameClass::NoiseHandshake {
                return Err(NoiseError::UnexpectedFrame);
            }
            let read = state
                .read_message(&frame.payload, &mut payload)
                .map_err(|_| NoiseError::HandshakeFailed)?;
            if read != 0 {
                return Err(NoiseError::HandshakeFailed);
            }
        }
        NoiseRole::Responder => {
            let frame = io.read_frame().await.map_err(NoiseError::Frame)?;
            if frame.class != FrameClass::NoiseHandshake {
                return Err(NoiseError::UnexpectedFrame);
            }
            let read = state
                .read_message(&frame.payload, &mut payload)
                .map_err(|_| NoiseError::HandshakeFailed)?;
            if read != 0 {
                return Err(NoiseError::HandshakeFailed);
            }

            let written = state
                .write_message(&[], &mut message)
                .map_err(|_| NoiseError::HandshakeFailed)?;
            io.write_frame(FrameClass::NoiseHandshake, &message[..written])
                .await
                .map_err(NoiseError::Frame)?;
        }
    }

    if !state.is_handshake_finished() {
        return Err(NoiseError::HandshakeFailed);
    }
    if state.get_remote_static() != Some(config.remote_static_public_key.as_slice()) {
        return Err(NoiseError::RemoteStaticMismatch);
    }
    let transport = state
        .into_transport_mode()
        .map_err(|_| NoiseError::HandshakeFailed)?;

    Ok(NoiseConnection {
        io,
        transport,
        local_device_id: config.local_device_id,
        remote_device_id: config.remote_device_id,
        role: config.role,
        terminal: false,
    })
}

fn validate_config(config: &NoiseConfig<'_>) -> Result<(), NoiseError> {
    if config.protocol_version != SYNC_PROTOCOL_VERSION {
        return Err(NoiseError::UnsupportedProtocol);
    }
    if config.local_device_id == config.remote_device_id {
        return Err(NoiseError::SameDevice);
    }
    // The caller owns socket direction. Device ordering is applied later by
    // `resolve_duplicate`, so a non-preferred connection must still complete
    // this authenticated handshake before it can be replaced.
    let local_private = config.local_static_keypair.private_key_bytes();
    let local_public = config.local_static_keypair.public_key();
    if local_private.iter().all(|byte| *byte == 0)
        || local_public.iter().all(|byte| *byte == 0)
        || config
            .remote_static_public_key
            .iter()
            .all(|byte| *byte == 0)
    {
        return Err(NoiseError::InvalidConfiguration("zero static key"));
    }
    Ok(())
}

fn noise_params() -> snow::params::NoiseParams {
    static PARAMS: OnceLock<snow::params::NoiseParams> = OnceLock::new();
    PARAMS
        .get_or_init(|| {
            NOISE_PATTERN
                .parse()
                .expect("fixed Noise pattern must parse")
        })
        .clone()
}

impl<S> NoiseConnection<S>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    #[must_use]
    pub fn local_device_id(&self) -> DeviceId {
        self.local_device_id
    }

    #[must_use]
    pub fn remote_device_id(&self) -> DeviceId {
        self.remote_device_id
    }

    #[must_use]
    pub fn role(&self) -> NoiseRole {
        self.role
    }

    pub async fn send(&mut self, plaintext: &[u8]) -> Result<(), NoiseError> {
        if self.terminal {
            return Err(NoiseError::Closed);
        }
        if plaintext.len() > MAX_NOISE_PLAINTEXT_BYTES {
            return Err(NoiseError::PlaintextTooLarge);
        }

        let mut message = vec![0_u8; MAX_NOISE_MESSAGE_BYTES];
        let written = match self.transport.write_message(plaintext, &mut message) {
            Ok(written) => written,
            Err(error) => {
                let mapped = map_encrypt_error(error);
                self.terminate().await;
                return Err(mapped);
            }
        };
        let result = self
            .io
            .write_frame(FrameClass::NoiseTransport, &message[..written])
            .await
            .map_err(NoiseError::Frame);
        if result.is_err() {
            self.terminate().await;
        }
        result
    }

    pub async fn receive(&mut self) -> Result<Vec<u8>, NoiseError> {
        if self.terminal {
            return Err(NoiseError::Closed);
        }
        let frame = match self.io.read_frame().await {
            Ok(frame) => frame,
            Err(error) => {
                self.terminate().await;
                return Err(NoiseError::Frame(error));
            }
        };
        if frame.class != FrameClass::NoiseTransport {
            self.terminate().await;
            return Err(NoiseError::UnexpectedFrame);
        }
        let mut plaintext = vec![0_u8; MAX_NOISE_PLAINTEXT_BYTES];
        let read = match self.transport.read_message(&frame.payload, &mut plaintext) {
            Ok(read) => read,
            Err(error) => {
                let mapped = map_decrypt_error(error);
                self.terminate().await;
                return Err(mapped);
            }
        };
        plaintext.truncate(read);
        Ok(plaintext)
    }

    pub async fn close(&mut self) -> Result<(), NoiseError> {
        if self.terminal {
            return Ok(());
        }
        self.terminal = true;
        self.io.shutdown().await.map_err(NoiseError::Frame)
    }

    async fn terminate(&mut self) {
        self.terminal = true;
        let _ = self.io.shutdown().await;
    }
}

fn map_encrypt_error(error: SnowError) -> NoiseError {
    if matches!(error, SnowError::State(StateProblem::Exhausted)) {
        NoiseError::NonceExhausted
    } else {
        NoiseError::EncryptFailed
    }
}

fn map_decrypt_error(error: SnowError) -> NoiseError {
    if matches!(error, SnowError::State(StateProblem::Exhausted)) {
        NoiseError::NonceExhausted
    } else {
        NoiseError::DecryptFailed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::TransportLimits;
    use std::time::Duration;
    use tokio::io::duplex;

    fn limits() -> TransportLimits {
        TransportLimits {
            connect_timeout: Duration::from_secs(1),
            handshake_timeout: Duration::from_secs(1),
            read_timeout: Duration::from_secs(1),
            write_timeout: Duration::from_secs(1),
            idle_timeout: Duration::from_secs(1),
            max_connections: 1,
        }
    }

    fn ids() -> (DeviceId, DeviceId) {
        (DeviceId::from_bytes([1; 32]), DeviceId::from_bytes([2; 32]))
    }

    fn configs<'a>(
        vault_id: VaultId,
        a_key: &'a X25519Keypair,
        b_key: &'a X25519Keypair,
    ) -> (NoiseConfig<'a>, NoiseConfig<'a>) {
        let (a_id, b_id) = ids();
        (
            NoiseConfig {
                role: NoiseRole::Initiator,
                protocol_version: SYNC_PROTOCOL_VERSION,
                vault_id,
                local_device_id: a_id,
                remote_device_id: b_id,
                local_static_keypair: a_key,
                remote_static_public_key: b_key.public_key(),
            },
            NoiseConfig {
                role: NoiseRole::Responder,
                protocol_version: SYNC_PROTOCOL_VERSION,
                vault_id,
                local_device_id: b_id,
                remote_device_id: a_id,
                local_static_keypair: b_key,
                remote_static_public_key: a_key.public_key(),
            },
        )
    }

    async fn connected_pair(
        vault_id: VaultId,
    ) -> (
        NoiseConnection<tokio::io::DuplexStream>,
        NoiseConnection<tokio::io::DuplexStream>,
    ) {
        let a_key = X25519Keypair::from_private_bytes([3; 32]);
        let b_key = X25519Keypair::from_private_bytes([4; 32]);
        let (left, right) = duplex(256);
        let left = FramedIo::new(left, limits()).unwrap();
        let right = FramedIo::new(right, limits()).unwrap();
        let (left_config, right_config) = configs(vault_id, &a_key, &b_key);
        let deadline = Instant::now() + Duration::from_secs(2);
        let (left, right) = tokio::join!(
            handshake(left, left_config, deadline),
            handshake(right, right_config, deadline)
        );
        (left.unwrap(), right.unwrap())
    }

    #[test]
    fn prologue_golden_vector() {
        let vault = VaultId::from_bytes([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
        assert_eq!(
            encode_noise_prologue(1, vault),
            [
                b'L', b'O', b'C', b'K', b'E', b'R', b'-', b'N', b'O', b'I', b'S', b'E', 0, 1, 0, 0,
                1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
            ]
        );
    }

    #[tokio::test]
    async fn kk_round_trip_in_both_directions_at_maximum_size() {
        let (mut initiator, mut responder) = connected_pair(VaultId::from_bytes([7; 16])).await;
        assert_eq!(initiator.role(), NoiseRole::Initiator);
        assert_eq!(responder.role(), NoiseRole::Responder);

        let (sent, received) = tokio::join!(initiator.send(&[]), responder.receive());
        sent.unwrap();
        assert!(received.unwrap().is_empty());

        let small = b"locker";
        let (sent, received) = tokio::join!(initiator.send(small), responder.receive());
        sent.unwrap();
        assert_eq!(received.unwrap(), small);

        let maximum = vec![0x5a; MAX_NOISE_PLAINTEXT_BYTES];
        let (sent, received) = tokio::join!(responder.send(&maximum), initiator.receive());
        sent.unwrap();
        assert_eq!(received.unwrap(), maximum);
    }

    #[tokio::test]
    async fn larger_device_can_initiate_and_complete_kk() {
        let a_key = X25519Keypair::from_private_bytes([3; 32]);
        let b_key = X25519Keypair::from_private_bytes([4; 32]);
        let (left, right) = duplex(256);
        let left = FramedIo::new(left, limits()).unwrap();
        let right = FramedIo::new(right, limits()).unwrap();
        let (a_id, b_id) = ids();
        let vault_id = VaultId::from_bytes([7; 16]);
        let left_config = NoiseConfig {
            role: NoiseRole::Responder,
            protocol_version: SYNC_PROTOCOL_VERSION,
            vault_id,
            local_device_id: a_id,
            remote_device_id: b_id,
            local_static_keypair: &a_key,
            remote_static_public_key: b_key.public_key(),
        };
        let right_config = NoiseConfig {
            role: NoiseRole::Initiator,
            protocol_version: SYNC_PROTOCOL_VERSION,
            vault_id,
            local_device_id: b_id,
            remote_device_id: a_id,
            local_static_keypair: &b_key,
            remote_static_public_key: a_key.public_key(),
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        let (left, right) = tokio::join!(
            handshake(left, left_config, deadline),
            handshake(right, right_config, deadline)
        );
        assert_eq!(left.unwrap().role(), NoiseRole::Responder);
        assert_eq!(right.unwrap().role(), NoiseRole::Initiator);
    }

    #[tokio::test]
    async fn wrong_vault_and_protocol_are_rejected_before_transport() {
        let a_key = X25519Keypair::from_private_bytes([3; 32]);
        let b_key = X25519Keypair::from_private_bytes([4; 32]);
        let (left, right) = duplex(256);
        let left = FramedIo::new(left, limits()).unwrap();
        let right = FramedIo::new(right, limits()).unwrap();
        let (left_config, mut right_config) = configs(VaultId::from_bytes([7; 16]), &a_key, &b_key);
        right_config.vault_id = VaultId::from_bytes([8; 16]);
        let deadline = Instant::now() + Duration::from_secs(2);
        let (left, right) = tokio::join!(
            handshake(left, left_config, deadline),
            handshake(right, right_config, deadline)
        );
        assert!(left.is_err());
        assert!(right.is_err());

        let (left, _right) = duplex(256);
        let left = FramedIo::new(left, limits()).unwrap();
        let (a_id, b_id) = ids();
        let invalid_version = NoiseConfig {
            role: NoiseRole::Initiator,
            protocol_version: SYNC_PROTOCOL_VERSION + 1,
            vault_id: VaultId::from_bytes([7; 16]),
            local_device_id: a_id,
            remote_device_id: b_id,
            local_static_keypair: &a_key,
            remote_static_public_key: b_key.public_key(),
        };
        assert!(matches!(
            handshake(
                left,
                invalid_version,
                Instant::now() + Duration::from_secs(1)
            )
            .await,
            Err(NoiseError::UnsupportedProtocol)
        ));
    }

    #[tokio::test]
    async fn both_initiators_fail_without_a_usable_connection() {
        let a_key = X25519Keypair::from_private_bytes([3; 32]);
        let b_key = X25519Keypair::from_private_bytes([4; 32]);
        let (left, right) = duplex(256);
        let left = FramedIo::new(left, limits()).unwrap();
        let right = FramedIo::new(right, limits()).unwrap();
        let (mut left_config, mut right_config) =
            configs(VaultId::from_bytes([7; 16]), &a_key, &b_key);
        left_config.role = NoiseRole::Initiator;
        right_config.role = NoiseRole::Initiator;
        let deadline = Instant::now() + Duration::from_secs(1);
        let (left, right) = tokio::join!(
            handshake(left, left_config, deadline),
            handshake(right, right_config, deadline)
        );
        assert!(left.is_err());
        assert!(right.is_err());
    }

    #[tokio::test]
    async fn tampered_handshake_message_fails_closed() {
        let a_key = X25519Keypair::from_private_bytes([3; 32]);
        let b_key = X25519Keypair::from_private_bytes([4; 32]);
        let (left, right) = duplex(256);
        let left = FramedIo::new(left, limits()).unwrap();
        let mut right = FramedIo::new(right, limits()).unwrap();
        let (left_config, _) = configs(VaultId::from_bytes([7; 16]), &a_key, &b_key);
        let deadline = Instant::now() + Duration::from_secs(1);
        let (result, _) = tokio::join!(handshake(left, left_config, deadline), async {
            let mut frame = right.read_frame().await.unwrap();
            frame.payload[0] ^= 1;
            right
                .write_frame(FrameClass::NoiseHandshake, &frame.payload)
                .await
                .unwrap();
        });
        assert!(matches!(result, Err(NoiseError::HandshakeFailed)));
    }

    #[tokio::test]
    async fn wrong_static_key_fails_without_a_connection() {
        let a_key = X25519Keypair::from_private_bytes([3; 32]);
        let b_key = X25519Keypair::from_private_bytes([4; 32]);
        let wrong_key = X25519Keypair::from_private_bytes([5; 32]);
        let (left, right) = duplex(256);
        let left = FramedIo::new(left, limits()).unwrap();
        let right = FramedIo::new(right, limits()).unwrap();
        let (mut left_config, right_config) = configs(VaultId::from_bytes([7; 16]), &a_key, &b_key);
        left_config.remote_static_public_key = wrong_key.public_key();
        let deadline = Instant::now() + Duration::from_secs(2);
        let (left, right) = tokio::join!(
            handshake(left, left_config, deadline),
            handshake(right, right_config, deadline)
        );
        assert!(left.is_err());
        assert!(right.is_err());
    }

    #[tokio::test]
    async fn max_plus_one_is_rejected_before_transport_write() {
        let (mut initiator, _responder) = connected_pair(VaultId::from_bytes([7; 16])).await;
        let oversized = vec![0_u8; MAX_NOISE_PLAINTEXT_BYTES + 1];
        assert!(matches!(
            initiator.send(&oversized).await,
            Err(NoiseError::PlaintextTooLarge)
        ));
        assert!(!initiator.terminal);
    }

    #[tokio::test]
    async fn close_makes_follow_up_operations_closed() {
        let a_key = X25519Keypair::from_private_bytes([3; 32]);
        let b_key = X25519Keypair::from_private_bytes([4; 32]);
        let (left, right) = duplex(256);
        let left = FramedIo::new(left, limits()).unwrap();
        let right = FramedIo::new(right, limits()).unwrap();
        let (left_config, right_config) = configs(VaultId::from_bytes([7; 16]), &a_key, &b_key);
        let deadline = Instant::now() + Duration::from_secs(2);
        let (initiator, responder) = tokio::join!(
            handshake(left, left_config, deadline),
            handshake(right, right_config, deadline)
        );
        let mut initiator = initiator.unwrap();
        let mut responder = responder.unwrap();
        let _ = initiator.close().await;
        assert!(matches!(
            initiator.send(b"closed").await,
            Err(NoiseError::Closed)
        ));
        assert!(matches!(
            responder.receive().await,
            Err(NoiseError::Frame(_))
        ));
    }

    #[tokio::test]
    async fn wrong_frame_class_terminates_the_handshake() {
        let a_key = X25519Keypair::from_private_bytes([3; 32]);
        let b_key = X25519Keypair::from_private_bytes([4; 32]);
        let (left, right) = duplex(256);
        let mut left = FramedIo::new(left, limits()).unwrap();
        let right = FramedIo::new(right, limits()).unwrap();
        let (_, right_config) = configs(VaultId::from_bytes([7; 16]), &a_key, &b_key);
        left.write_frame(FrameClass::Pairing, &[]).await.unwrap();
        let result = handshake(right, right_config, Instant::now() + Duration::from_secs(1)).await;
        assert!(matches!(result, Err(NoiseError::UnexpectedFrame)));
    }

    #[test]
    fn duplicate_resolution_is_order_independent_and_rejects_same_device() {
        let (a, b) = ids();
        assert_eq!(preferred_initiator(a, b).unwrap(), a);
        assert_eq!(preferred_initiator(b, a).unwrap(), a);
        assert_eq!(
            resolve_duplicate(
                a,
                b,
                SessionDirection::RemotelyInitiated,
                SessionDirection::LocallyInitiated
            )
            .unwrap(),
            DuplicateDecision::KeepCandidate
        );
        assert_eq!(
            resolve_duplicate(
                b,
                a,
                SessionDirection::LocallyInitiated,
                SessionDirection::RemotelyInitiated
            )
            .unwrap(),
            DuplicateDecision::KeepCandidate
        );
        assert!(matches!(
            preferred_initiator(a, a),
            Err(NoiseError::SameDevice)
        ));
    }

    #[test]
    fn duplicate_resolution_exhausts_direction_matrix_from_both_peers() {
        let (a, b) = ids();
        let directions = [
            SessionDirection::LocallyInitiated,
            SessionDirection::RemotelyInitiated,
        ];
        for (existing, candidate) in directions.into_iter().flat_map(|existing| {
            directions
                .into_iter()
                .map(move |candidate| (existing, candidate))
        }) {
            let expected_for_a = if existing == SessionDirection::LocallyInitiated {
                DuplicateDecision::KeepExisting
            } else if candidate == SessionDirection::LocallyInitiated {
                DuplicateDecision::KeepCandidate
            } else {
                DuplicateDecision::KeepExisting
            };
            assert_eq!(
                resolve_duplicate(a, b, existing, candidate).unwrap(),
                expected_for_a
            );

            let expected_for_b = if existing == SessionDirection::RemotelyInitiated {
                DuplicateDecision::KeepExisting
            } else if candidate == SessionDirection::RemotelyInitiated {
                DuplicateDecision::KeepCandidate
            } else {
                DuplicateDecision::KeepExisting
            };
            assert_eq!(
                resolve_duplicate(b, a, existing, candidate).unwrap(),
                expected_for_b
            );
        }
    }

    #[tokio::test]
    async fn handshake_deadline_covers_a_stalled_peer() {
        let (left, _right) = duplex(256);
        let left = FramedIo::new(left, limits()).unwrap();
        let key = X25519Keypair::from_private_bytes([3; 32]);
        let peer = X25519Keypair::from_private_bytes([4; 32]);
        let (a_id, b_id) = ids();
        let config = NoiseConfig {
            role: NoiseRole::Initiator,
            protocol_version: SYNC_PROTOCOL_VERSION,
            vault_id: VaultId::from_bytes([7; 16]),
            local_device_id: a_id,
            remote_device_id: b_id,
            local_static_keypair: &key,
            remote_static_public_key: peer.public_key(),
        };
        let deadline = Instant::now() + Duration::from_millis(20);
        assert!(matches!(
            handshake(left, config, deadline).await,
            Err(NoiseError::HandshakeTimeout)
        ));
    }
}
