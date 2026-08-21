//! Bounded SPAKE2 pairing and one-shot device onboarding.
//!
//! The module intentionally owns the wire codec and crypto session.  SQLite
//! work stays behind `locker_core::PairingStoreHandle` and is run in a
//! blocking task by the state machines below.

use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use locker_core::{
    DeviceId, DeviceIdentity, Hlc, MAX_MEMBERSHIP_RECORD_BYTES, MAX_MEMBERSHIP_RECORDS,
    MembershipAcceptance, MembershipRecord, MembershipRecordHash, PairingStore,
    PairingVaultPackage, SecretBytes, Vault, VaultError, VaultId,
};
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use std::{
    fmt,
    net::SocketAddr,
    path::PathBuf,
    str::FromStr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::{self, Instant};
use zeroize::{Zeroize, Zeroizing};

use crate::{
    MAX_FRAME_PAYLOAD_BYTES,
    frame::{FrameClass, FrameError},
    transport::FramedIo,
};

pub const PAIRING_LIFETIME: Duration = Duration::from_secs(5 * 60);
pub const MAX_FAILED_ATTEMPTS: u8 = 5;
pub const PAIRING_LOCATOR_SYMBOLS: usize = 5;
pub const PAIRING_SECRET_SYMBOLS: usize = 10;
pub const PAIRING_INSTANCE_BYTES: usize = 16;
pub const CONFIRMATION_TAG_BYTES: usize = 32;
pub const PAIRING_AEAD_TAG_BYTES: usize = 16;
pub const MAX_PAIRING_PLAINTEXT_BYTES: usize = 60 * 1024;

const VERSION: u16 = 1;
const MAX_SPAKE_BYTES: usize = 128;
const CHUNK_BYTES: usize = 48 * 1024;
const MAX_TRANSFER_BYTES: usize = MAX_MEMBERSHIP_RECORDS * MAX_MEMBERSHIP_RECORD_BYTES;
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PairingInstanceId([u8; PAIRING_INSTANCE_BYTES]);

impl PairingInstanceId {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; PAIRING_INSTANCE_BYTES]) -> Self {
        Self(bytes)
    }
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; PAIRING_INSTANCE_BYTES] {
        &self.0
    }
}
impl AsRef<[u8]> for PairingInstanceId {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

/// Ten canonical Crockford symbols.  Storing symbols avoids a second
/// conversion at the PAKE boundary and keeps the owner zeroizing.
#[derive(Eq, PartialEq)]
pub struct PairingSecret(Zeroizing<[u8; PAIRING_SECRET_SYMBOLS]>);
impl Clone for PairingSecret {
    fn clone(&self) -> Self {
        Self(Zeroizing::new(*self.0))
    }
}
impl PairingSecret {
    fn random() -> Result<Self, PairingError> {
        let mut out = [0_u8; PAIRING_SECRET_SYMBOLS];
        for symbol in &mut out {
            loop {
                let mut byte = [0_u8; 1];
                getrandom::fill(&mut byte).map_err(|_| PairingError::Storage)?;
                if byte[0] < 224 {
                    *symbol = ALPHABET[(byte[0] & 31) as usize];
                    break;
                }
            }
        }
        Ok(Self(Zeroizing::new(out)))
    }
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; PAIRING_SECRET_SYMBOLS] {
        &self.0
    }
}
impl fmt::Debug for PairingSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairingSecret(<redacted>)")
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct PairingCodeDisplay(Zeroizing<String>);
impl PairingCodeDisplay {
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}
impl fmt::Display for PairingCodeDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl fmt::Debug for PairingCodeDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairingCodeDisplay(<redacted>)")
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct PairingCodeInput {
    locator: [u8; PAIRING_LOCATOR_SYMBOLS],
    secret: PairingSecret,
}
impl PairingCodeInput {
    #[must_use]
    pub fn locator(&self) -> &[u8; PAIRING_LOCATOR_SYMBOLS] {
        &self.locator
    }
    #[must_use]
    pub fn secret(&self) -> &PairingSecret {
        &self.secret
    }
}
impl fmt::Debug for PairingCodeInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairingCodeInput(<redacted>)")
    }
}
impl FromStr for PairingCodeInput {
    type Err = PairingError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut symbols = Zeroizing::new([0_u8; 15]);
        let mut count = 0;
        let mut hyphens = 0;
        for byte in value.bytes() {
            if byte == b'-' {
                if (hyphens == 0 && count != 5) || (hyphens == 1 && count != 10) {
                    return Err(PairingError::InvalidInput);
                }
                hyphens += 1;
                continue;
            }
            let upper = byte.to_ascii_uppercase();
            if !ALPHABET.contains(&upper) {
                return Err(PairingError::InvalidInput);
            }
            if count == symbols.len() {
                return Err(PairingError::InvalidInput);
            }
            symbols[count] = upper;
            count += 1;
        }
        if count != symbols.len() || (hyphens != 0 && hyphens != 2) {
            return Err(PairingError::InvalidInput);
        }
        let locator: [u8; 5] = symbols[..5]
            .try_into()
            .map_err(|_| PairingError::InvalidInput)?;
        let secret: [u8; 10] = symbols[5..]
            .try_into()
            .map_err(|_| PairingError::InvalidInput)?;
        Ok(Self {
            locator,
            secret: PairingSecret(Zeroizing::new(secret)),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OfferState {
    Available,
    InProgress,
    Consumed,
    LockedOut,
    Expired,
    Cancelled,
}

pub struct PairingOffer {
    pub instance_id: PairingInstanceId,
    pub display_code: PairingCodeDisplay,
    pub expires_at: Instant,
    secret: PairingSecret,
    attempts: u8,
    state: OfferState,
}
impl fmt::Debug for PairingOffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairingOffer")
            .field("instance_id", &self.instance_id)
            .field("display_code", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("attempts", &self.attempts)
            .field("state", &self.state)
            .finish()
    }
}

impl PairingOffer {
    #[must_use]
    pub fn expires_at(&self) -> Instant {
        self.expires_at
    }
}

/// A discovery result carrying the full instance identifier.  The five-symbol
/// locator is only a prefilter; Task G must select exactly one full ID before
/// this target reaches the PAKE state machine, so locator collisions never
/// cause attempts against multiple offers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PairingTarget {
    pub instance_id: PairingInstanceId,
    pub inviter_endpoint: SocketAddr,
}

pub struct JoinRequest {
    pub code: PairingCodeInput,
    pub destination: PathBuf,
    pub master_password: SecretBytes,
    pub display_name: String,
}
impl fmt::Debug for JoinRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinRequest")
            .field("code", &"<redacted>")
            .field("destination", &self.destination)
            .field("master_password", &"<redacted>")
            .field("display_name", &self.display_name)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PairingOutcome {
    pub vault_id: VaultId,
    pub inviter_device_id: DeviceId,
    pub admitted_device_id: DeviceId,
    pub admission_hash: MembershipRecordHash,
}

#[derive(Clone)]
pub struct PairingStoreHandle(Arc<Mutex<PairingStore>>);
impl PairingStoreHandle {
    #[must_use]
    pub fn new(store: PairingStore) -> Self {
        Self(Arc::new(Mutex::new(store)))
    }
}
impl fmt::Debug for PairingStoreHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairingStoreHandle(<redacted>)")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingError {
    InvalidInput,
    Unavailable,
    Expired,
    Cancelled,
    AttemptsExceeded,
    ProtocolMismatch,
    AuthenticationFailed,
    InvalidPeerIdentity,
    InvalidMembership,
    TransferTooLarge,
    ReplayOrOrdering,
    Storage,
    Transport,
}
impl fmt::Display for PairingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidInput => "invalid pairing input",
            Self::Unavailable => "pairing unavailable",
            Self::Expired => "pairing expired",
            Self::Cancelled => "pairing cancelled",
            Self::AttemptsExceeded => "pairing attempts exceeded",
            Self::ProtocolMismatch => "pairing protocol mismatch",
            Self::AuthenticationFailed => "pairing authentication failed",
            Self::InvalidPeerIdentity => "invalid peer identity",
            Self::InvalidMembership => "invalid pairing membership",
            Self::TransferTooLarge => "pairing transfer too large",
            Self::ReplayOrOrdering => "pairing message ordering failure",
            Self::Storage => "pairing storage error",
            Self::Transport => "pairing transport error",
        })
    }
}
impl std::error::Error for PairingError {}
impl From<FrameError> for PairingError {
    fn from(_: FrameError) -> Self {
        Self::Transport
    }
}
impl From<VaultError> for PairingError {
    fn from(_: VaultError) -> Self {
        Self::Storage
    }
}

#[must_use]
pub fn locator_for(instance: PairingInstanceId) -> [u8; 5] {
    let mut h = Sha256::new();
    h.update(b"LOCKER-PAIR-LOCATOR\0");
    h.update(instance.0);
    let digest = h.finalize();
    let value = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) >> 7;
    let mut out = [0; 5];
    for (index, item) in out.iter_mut().enumerate() {
        *item = ALPHABET[((value >> (20 - index * 5)) & 31) as usize];
    }
    out
}

pub fn create_offer(now: Instant) -> Result<PairingOffer, PairingError> {
    let mut id = [0_u8; 16];
    getrandom::fill(&mut id).map_err(|_| PairingError::Storage)?;
    let instance_id = PairingInstanceId(id);
    let secret = PairingSecret::random()?;
    let locator = locator_for(instance_id);
    let mut code = String::with_capacity(17);
    code.extend(locator.into_iter().map(char::from));
    code.push('-');
    for (index, symbol) in secret.as_bytes().iter().enumerate() {
        if index == 5 {
            code.push('-');
        }
        code.push(char::from(*symbol));
    }
    Ok(PairingOffer {
        instance_id,
        display_code: PairingCodeDisplay(Zeroizing::new(code)),
        expires_at: now + PAIRING_LIFETIME,
        secret,
        attempts: 0,
        state: OfferState::Available,
    })
}

pub fn cancel_offer(offer: &mut PairingOffer) {
    if matches!(offer.state, OfferState::Available | OfferState::InProgress) {
        offer.state = OfferState::Cancelled;
        clear_offer_secrets(offer);
    }
}

fn clear_offer_secrets(offer: &mut PairingOffer) {
    offer.secret.0.zeroize();
    offer.display_code.0.zeroize();
}

fn id_bytes(role: u8, instance: PairingInstanceId) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + 2 + 16);
    out.extend_from_slice(if role == 0 {
        b"LOCKER-PAIR-A\0"
    } else {
        b"LOCKER-PAIR-B\0"
    });
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&instance.0);
    out
}
fn transcript(instance: PairingInstanceId, a: &[u8], b: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"LOCKER-PAIR-TRANSCRIPT\0");
    h.update(VERSION.to_le_bytes());
    h.update(instance.0);
    let id_a = id_bytes(0, instance);
    let id_b = id_bytes(1, instance);
    for part in [&id_a[..], &id_b[..], a, b] {
        h.update(part);
    }
    h.finalize().into()
}

struct PairingKeys {
    confirm_a: Zeroizing<[u8; 32]>,
    confirm_b: Zeroizing<[u8; 32]>,
    a_to_b: Zeroizing<[u8; 32]>,
    b_to_a: Zeroizing<[u8; 32]>,
    nonce_a_to_b: Zeroizing<[u8; 16]>,
    nonce_b_to_a: Zeroizing<[u8; 16]>,
}
fn derive_keys(
    instance: PairingInstanceId,
    transcript_hash: [u8; 32],
    shared: &[u8],
) -> Result<PairingKeys, PairingError> {
    let hk = Hkdf::<Sha256>::new(Some(&transcript_hash), shared);
    let get = |label: &[u8], n: usize| -> Result<Vec<u8>, PairingError> {
        let mut info = Vec::with_capacity(32);
        info.extend_from_slice(b"LOCKER-PAIR-KEYS\0");
        info.extend_from_slice(&VERSION.to_le_bytes());
        info.extend_from_slice(&instance.0);
        info.extend_from_slice(label);
        let mut out = vec![0; n];
        hk.expand(&info, &mut out)
            .map_err(|_| PairingError::AuthenticationFailed)?;
        Ok(out)
    };
    Ok(PairingKeys {
        confirm_a: Zeroizing::new(get(b"confirm-a", 32)?.try_into().unwrap()),
        confirm_b: Zeroizing::new(get(b"confirm-b", 32)?.try_into().unwrap()),
        a_to_b: Zeroizing::new(get(b"encrypt-a-to-b", 32)?.try_into().unwrap()),
        b_to_a: Zeroizing::new(get(b"encrypt-b-to-a", 32)?.try_into().unwrap()),
        nonce_a_to_b: Zeroizing::new(get(b"nonce-a-to-b", 16)?.try_into().unwrap()),
        nonce_b_to_a: Zeroizing::new(get(b"nonce-b-to-a", 16)?.try_into().unwrap()),
    })
}
fn confirmation(key: &[u8], hash: &[u8; 32], role: u8) -> [u8; 32] {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC accepts 32-byte keys");
    mac.update(hash);
    mac.update(&[role]);
    mac.finalize().into_bytes().into()
}

#[derive(Clone, Copy)]
enum Role {
    A,
    B,
}
struct Session {
    instance: PairingInstanceId,
    role: Role,
    keys: PairingKeys,
    send: u64,
    recv: u64,
}
impl Session {
    fn seal(&mut self, tag: u8, plaintext: &[u8]) -> Result<(u64, Vec<u8>), PairingError> {
        if plaintext.len() > MAX_PAIRING_PLAINTEXT_BYTES || self.send == u64::MAX {
            return Err(PairingError::TransferTooLarge);
        }
        let seq = self.send;
        let (key, prefix, direction) = match self.role {
            Role::A => (&self.keys.a_to_b, &self.keys.nonce_a_to_b, 0),
            Role::B => (&self.keys.b_to_a, &self.keys.nonce_b_to_a, 1),
        };
        let nonce = nonce(prefix, seq);
        let aad = aad(self.instance, direction, seq, tag);
        let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref())
            .map_err(|_| PairingError::AuthenticationFailed)?;
        let ciphertext = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| PairingError::AuthenticationFailed)?;
        self.send += 1;
        Ok((seq, ciphertext))
    }
    fn open(&mut self, tag: u8, seq: u64, ciphertext: &[u8]) -> Result<Vec<u8>, PairingError> {
        if self.recv == u64::MAX || seq != self.recv {
            return Err(PairingError::ReplayOrOrdering);
        }
        if ciphertext.len() < PAIRING_AEAD_TAG_BYTES
            || ciphertext.len() > MAX_PAIRING_PLAINTEXT_BYTES + PAIRING_AEAD_TAG_BYTES
        {
            return Err(PairingError::TransferTooLarge);
        }
        let (key, prefix, direction) = match self.role {
            Role::A => (&self.keys.b_to_a, &self.keys.nonce_b_to_a, 1),
            Role::B => (&self.keys.a_to_b, &self.keys.nonce_a_to_b, 0),
        };
        let nonce = nonce(prefix, seq);
        let aad = aad(self.instance, direction, seq, tag);
        let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref())
            .map_err(|_| PairingError::AuthenticationFailed)?;
        let plain = cipher
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| PairingError::AuthenticationFailed)?;
        self.recv += 1;
        Ok(plain)
    }
}
fn nonce(prefix: &[u8; 16], seq: u64) -> [u8; 24] {
    let mut n = [0; 24];
    n[..16].copy_from_slice(prefix);
    n[16..].copy_from_slice(&seq.to_le_bytes());
    n
}
fn aad(instance: PairingInstanceId, direction: u8, seq: u64, tag: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(40);
    out.extend_from_slice(b"LOCKER-PAIR-AEAD\0");
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend_from_slice(&instance.0);
    out.push(direction);
    out.extend_from_slice(&seq.to_le_bytes());
    out.push(tag);
    out
}

enum Message {
    ClientHello {
        instance_id: PairingInstanceId,
        protocol_version: u16,
    },
    SpakeA(Zeroizing<Vec<u8>>),
    SpakeB(Zeroizing<Vec<u8>>),
    ConfirmA([u8; 32]),
    ConfirmB([u8; 32]),
    Secure {
        tag: u8,
        seq: u64,
        ciphertext: Vec<u8>,
    },
    Abort(u8),
}
const T_CLIENT_HELLO: u8 = 1;
const T_SPAKE_A: u8 = 2;
const T_SPAKE_B: u8 = 3;
const T_CONFIRM_A: u8 = 4;
const T_CONFIRM_B: u8 = 5;
const T_IDENTITY: u8 = 6;
const T_HEADER: u8 = 7;
const T_CHUNK: u8 = 8;
const T_ACCEPTANCE: u8 = 9;
const T_FINAL: u8 = 10;
const T_ABORT: u8 = 11;
fn put_u16(out: &mut Vec<u8>, n: usize) -> Result<(), PairingError> {
    out.extend_from_slice(
        &u16::try_from(n)
            .map_err(|_| PairingError::TransferTooLarge)?
            .to_le_bytes(),
    );
    Ok(())
}
fn encode_message(message: &Message) -> Result<Zeroizing<Vec<u8>>, PairingError> {
    let (tag, body) = match message {
        Message::ClientHello {
            instance_id,
            protocol_version,
        } => {
            let mut body = instance_id.0.to_vec();
            body.extend_from_slice(&protocol_version.to_le_bytes());
            (T_CLIENT_HELLO, body)
        }
        Message::SpakeA(v) => (T_SPAKE_A, {
            let mut b = Vec::new();
            put_u16(&mut b, v.len())?;
            b.extend(v.as_slice());
            b
        }),
        Message::SpakeB(v) => (T_SPAKE_B, {
            let mut b = Vec::new();
            put_u16(&mut b, v.len())?;
            b.extend(v.as_slice());
            b
        }),
        Message::ConfirmA(v) => (T_CONFIRM_A, v.to_vec()),
        Message::ConfirmB(v) => (T_CONFIRM_B, v.to_vec()),
        Message::Abort(r) => (T_ABORT, vec![*r]),
        Message::Secure {
            tag,
            seq,
            ciphertext,
        } => {
            let mut b = Vec::new();
            b.extend_from_slice(&seq.to_le_bytes());
            let max = MAX_FRAME_PAYLOAD_BYTES.saturating_sub(1 + 2 + 8 + 4);
            if ciphertext.len() > max {
                return Err(PairingError::TransferTooLarge);
            }
            b.extend_from_slice(&(ciphertext.len() as u32).to_le_bytes());
            b.extend(ciphertext);
            (*tag, b)
        }
    };
    let body = Zeroizing::new(body);
    let mut out = Zeroizing::new(Vec::with_capacity(3 + body.len()));
    out.push(tag);
    out.extend_from_slice(&VERSION.to_le_bytes());
    out.extend(body.as_slice());
    Ok(out)
}
fn take<'a>(input: &'a [u8], cursor: &mut usize, n: usize) -> Result<&'a [u8], PairingError> {
    let end = cursor
        .checked_add(n)
        .ok_or(PairingError::ProtocolMismatch)?;
    let part = input
        .get(*cursor..end)
        .ok_or(PairingError::ProtocolMismatch)?;
    *cursor = end;
    Ok(part)
}
fn decode_message(input: &[u8]) -> Result<Message, PairingError> {
    if input.len() < 3 {
        return Err(PairingError::ProtocolMismatch);
    }
    let tag = input[0];
    if u16::from_le_bytes([input[1], input[2]]) != VERSION {
        return Err(PairingError::ProtocolMismatch);
    }
    let mut c = 3;
    let msg = match tag {
        T_CLIENT_HELLO => Message::ClientHello {
            instance_id: PairingInstanceId(
                take(input, &mut c, 16)?
                    .try_into()
                    .map_err(|_| PairingError::ProtocolMismatch)?,
            ),
            protocol_version: u16::from_le_bytes(
                take(input, &mut c, 2)?
                    .try_into()
                    .map_err(|_| PairingError::ProtocolMismatch)?,
            ),
        },
        T_SPAKE_A => Message::SpakeA(Zeroizing::new(read_var(input, &mut c, MAX_SPAKE_BYTES)?)),
        T_SPAKE_B => Message::SpakeB(Zeroizing::new(read_var(input, &mut c, MAX_SPAKE_BYTES)?)),
        T_CONFIRM_A => Message::ConfirmA(
            take(input, &mut c, 32)?
                .try_into()
                .map_err(|_| PairingError::ProtocolMismatch)?,
        ),
        T_CONFIRM_B => Message::ConfirmB(
            take(input, &mut c, 32)?
                .try_into()
                .map_err(|_| PairingError::ProtocolMismatch)?,
        ),
        T_ABORT => Message::Abort(
            *take(input, &mut c, 1)?
                .first()
                .ok_or(PairingError::ProtocolMismatch)?,
        ),
        T_IDENTITY | T_HEADER | T_CHUNK | T_ACCEPTANCE | T_FINAL => {
            let seq = u64::from_le_bytes(
                take(input, &mut c, 8)?
                    .try_into()
                    .map_err(|_| PairingError::ProtocolMismatch)?,
            );
            let len = u32::from_le_bytes(
                take(input, &mut c, 4)?
                    .try_into()
                    .map_err(|_| PairingError::ProtocolMismatch)?,
            ) as usize;
            if len > MAX_PAIRING_PLAINTEXT_BYTES + 16 {
                return Err(PairingError::TransferTooLarge);
            }
            Message::Secure {
                tag,
                seq,
                ciphertext: take(input, &mut c, len)?.to_vec(),
            }
        }
        _ => return Err(PairingError::ProtocolMismatch),
    };
    if c != input.len() {
        return Err(PairingError::ProtocolMismatch);
    }
    Ok(msg)
}
fn read_var(input: &[u8], c: &mut usize, max: usize) -> Result<Vec<u8>, PairingError> {
    let len = u16::from_le_bytes(
        take(input, c, 2)?
            .try_into()
            .map_err(|_| PairingError::ProtocolMismatch)?,
    ) as usize;
    if len == 0 || len > max {
        return Err(PairingError::AuthenticationFailed);
    };
    Ok(take(input, c, len)?.to_vec())
}
async fn send<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    io: &mut FramedIo<S>,
    msg: &Message,
) -> Result<(), PairingError> {
    let bytes = encode_message(msg)?;
    io.write_frame(FrameClass::Pairing, &bytes)
        .await
        .map_err(Into::into)
}
async fn receive<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    io: &mut FramedIo<S>,
) -> Result<Message, PairingError> {
    let frame = io.read_frame().await?;
    if frame.class != FrameClass::Pairing {
        return Err(PairingError::ProtocolMismatch);
    }
    let payload = Zeroizing::new(frame.payload);
    decode_message(&payload)
}

fn encode_identity(identity: &DeviceIdentity) -> Result<Vec<u8>, PairingError> {
    identity
        .verify()
        .map_err(|_| PairingError::InvalidPeerIdentity)?;
    let mut out = Vec::with_capacity(32 + 32 + 32 + 2 + identity.display_name.len() + 2 + 64);
    out.extend_from_slice(identity.device_id.as_ref());
    out.extend_from_slice(&identity.ed25519_public_key);
    out.extend_from_slice(&identity.x25519_public_key);
    put_u16(&mut out, identity.display_name.len())?;
    out.extend_from_slice(identity.display_name.as_bytes());
    out.extend_from_slice(&identity.protocol_version.to_le_bytes());
    out.extend_from_slice(&identity.self_signature);
    if out.len() > MAX_PAIRING_PLAINTEXT_BYTES {
        return Err(PairingError::TransferTooLarge);
    }
    Ok(out)
}

fn decode_identity(input: &[u8]) -> Result<DeviceIdentity, PairingError> {
    let mut c = 0;
    let device_id = DeviceId::from_bytes(
        take(input, &mut c, 32)?
            .try_into()
            .map_err(|_| PairingError::InvalidPeerIdentity)?,
    );
    let ed25519_public_key = take(input, &mut c, 32)?
        .try_into()
        .map_err(|_| PairingError::InvalidPeerIdentity)?;
    let x25519_public_key = take(input, &mut c, 32)?
        .try_into()
        .map_err(|_| PairingError::InvalidPeerIdentity)?;
    let display_len = u16::from_le_bytes(
        take(input, &mut c, 2)?
            .try_into()
            .map_err(|_| PairingError::InvalidPeerIdentity)?,
    ) as usize;
    if display_len > 128 {
        return Err(PairingError::InvalidPeerIdentity);
    }
    let display_name = String::from_utf8(take(input, &mut c, display_len)?.to_vec())
        .map_err(|_| PairingError::InvalidPeerIdentity)?;
    let protocol_version = u16::from_le_bytes(
        take(input, &mut c, 2)?
            .try_into()
            .map_err(|_| PairingError::InvalidPeerIdentity)?,
    );
    let self_signature = take(input, &mut c, 64)?
        .try_into()
        .map_err(|_| PairingError::InvalidPeerIdentity)?;
    if c != input.len() {
        return Err(PairingError::InvalidPeerIdentity);
    }
    let identity = DeviceIdentity {
        device_id,
        ed25519_public_key,
        x25519_public_key,
        display_name,
        protocol_version,
        self_signature,
    };
    identity
        .verify()
        .map_err(|_| PairingError::InvalidPeerIdentity)?;
    Ok(identity)
}

fn encode_records(records: &[MembershipRecord]) -> Result<Vec<u8>, PairingError> {
    if records.len() > MAX_MEMBERSHIP_RECORDS {
        return Err(PairingError::TransferTooLarge);
    }
    let mut out = Vec::with_capacity(records.len().saturating_mul(64));
    out.extend_from_slice(&(records.len() as u32).to_le_bytes());
    for record in records {
        let bytes = record
            .to_canonical_bytes()
            .map_err(|_| PairingError::InvalidMembership)?;
        if bytes.len() > MAX_MEMBERSHIP_RECORD_BYTES {
            return Err(PairingError::TransferTooLarge);
        }
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(&bytes);
        if out.len() > MAX_TRANSFER_BYTES {
            return Err(PairingError::TransferTooLarge);
        }
    }
    Ok(out)
}

fn decode_records(
    input: &[u8],
    expected_count: usize,
) -> Result<Vec<MembershipRecord>, PairingError> {
    if input.len() > MAX_TRANSFER_BYTES || input.len() < 4 {
        return Err(PairingError::TransferTooLarge);
    }
    let mut c = 0;
    let count = u32::from_le_bytes(
        take(input, &mut c, 4)?
            .try_into()
            .map_err(|_| PairingError::InvalidMembership)?,
    ) as usize;
    if count != expected_count || count > MAX_MEMBERSHIP_RECORDS {
        return Err(PairingError::InvalidMembership);
    }
    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        let len = u32::from_le_bytes(
            take(input, &mut c, 4)?
                .try_into()
                .map_err(|_| PairingError::InvalidMembership)?,
        ) as usize;
        if len > MAX_MEMBERSHIP_RECORD_BYTES {
            return Err(PairingError::TransferTooLarge);
        }
        let bytes = take(input, &mut c, len)?;
        records.push(
            MembershipRecord::from_canonical_bytes(bytes)
                .map_err(|_| PairingError::InvalidMembership)?,
        );
    }
    if c != input.len() {
        return Err(PairingError::InvalidMembership);
    }
    Ok(records)
}

fn encode_header(
    package: &PairingVaultPackage,
    record_count: usize,
    total_bytes: usize,
) -> Result<Vec<u8>, PairingError> {
    if record_count > MAX_MEMBERSHIP_RECORDS || total_bytes > MAX_TRANSFER_BYTES {
        return Err(PairingError::TransferTooLarge);
    }
    let mut out = Vec::with_capacity(16 + 32 + 32 + 32 + 8);
    out.extend_from_slice(package.vault_id.as_ref());
    out.extend_from_slice(package.admission_hash.as_ref());
    out.extend_from_slice(package.inviter_device_id.as_ref());
    out.extend_from_slice(package.dek.as_bytes());
    out.extend_from_slice(&(record_count as u32).to_le_bytes());
    out.extend_from_slice(&(total_bytes as u32).to_le_bytes());
    Ok(out)
}

struct TransferHeader {
    vault_id: VaultId,
    admission_hash: MembershipRecordHash,
    inviter_device_id: DeviceId,
    dek: Zeroizing<[u8; 32]>,
    record_count: usize,
    total_bytes: usize,
}
fn decode_header(input: &[u8]) -> Result<TransferHeader, PairingError> {
    if input.len() != 16 + 32 + 32 + 32 + 8 {
        return Err(PairingError::InvalidMembership);
    }
    let mut c = 0;
    let vault_id = VaultId::from_bytes(
        take(input, &mut c, 16)?
            .try_into()
            .map_err(|_| PairingError::InvalidMembership)?,
    );
    let admission_hash = MembershipRecordHash::from_bytes(
        take(input, &mut c, 32)?
            .try_into()
            .map_err(|_| PairingError::InvalidMembership)?,
    );
    let inviter = DeviceId::from_bytes(
        take(input, &mut c, 32)?
            .try_into()
            .map_err(|_| PairingError::InvalidMembership)?,
    );
    let dek = take(input, &mut c, 32)?
        .try_into()
        .map_err(|_| PairingError::InvalidMembership)?;
    let count = u32::from_le_bytes(
        take(input, &mut c, 4)?
            .try_into()
            .map_err(|_| PairingError::InvalidMembership)?,
    ) as usize;
    let total = u32::from_le_bytes(
        take(input, &mut c, 4)?
            .try_into()
            .map_err(|_| PairingError::InvalidMembership)?,
    ) as usize;
    if count == 0 || count > MAX_MEMBERSHIP_RECORDS || total > MAX_TRANSFER_BYTES {
        return Err(PairingError::TransferTooLarge);
    }
    Ok(TransferHeader {
        vault_id,
        admission_hash,
        inviter_device_id: inviter,
        dek: Zeroizing::new(dek),
        record_count: count,
        total_bytes: total,
    })
}

fn encode_acceptance(acceptance: &MembershipAcceptance) -> Result<Vec<u8>, PairingError> {
    MembershipRecord::Acceptance(acceptance.clone())
        .to_canonical_bytes()
        .map_err(|_| PairingError::InvalidMembership)
}
fn decode_acceptance(input: &[u8]) -> Result<MembershipAcceptance, PairingError> {
    match MembershipRecord::from_canonical_bytes(input)
        .map_err(|_| PairingError::InvalidMembership)?
    {
        MembershipRecord::Acceptance(value) => Ok(value),
        _ => Err(PairingError::InvalidMembership),
    }
}

fn verify_confirmation(
    key: &[u8],
    hash: &[u8; 32],
    role: u8,
    received: &[u8; 32],
) -> Result<(), PairingError> {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key)
        .map_err(|_| PairingError::AuthenticationFailed)?;
    mac.update(hash);
    mac.update(&[role]);
    mac.verify_slice(received)
        .map_err(|_| PairingError::AuthenticationFailed)
}

fn start_a(
    secret: &PairingSecret,
    instance: PairingInstanceId,
) -> (Spake2<Ed25519Group>, Zeroizing<Vec<u8>>) {
    let (spake, message) = Spake2::<Ed25519Group>::start_a(
        &Password::new(secret.as_bytes()),
        &Identity::new(&id_bytes(0, instance)),
        &Identity::new(&id_bytes(1, instance)),
    );
    (spake, Zeroizing::new(message))
}
fn start_b(
    secret: &PairingSecret,
    instance: PairingInstanceId,
) -> (Spake2<Ed25519Group>, Zeroizing<Vec<u8>>) {
    let (spake, message) = Spake2::<Ed25519Group>::start_b(
        &Password::new(secret.as_bytes()),
        &Identity::new(&id_bytes(0, instance)),
        &Identity::new(&id_bytes(1, instance)),
    );
    (spake, Zeroizing::new(message))
}

async fn prepare_offer(
    handle: &PairingStoreHandle,
    identity: DeviceIdentity,
) -> Result<PairingVaultPackage, PairingError> {
    let handle = handle.clone();
    crate::spawn_core_blocking(move || {
        let mut store = handle.0.lock().map_err(|_| PairingError::Storage)?;
        store
            .prepare_pairing_offer(identity, wall_hlc())
            .map_err(|_| PairingError::InvalidMembership)
    })
    .await
    .map_err(|_| PairingError::Storage)?
}
async fn finalize_acceptance(
    handle: &PairingStoreHandle,
    acceptance: MembershipAcceptance,
) -> Result<(), PairingError> {
    let handle = handle.clone();
    crate::spawn_core_blocking(move || {
        let mut store = handle.0.lock().map_err(|_| PairingError::Storage)?;
        store
            .finalize_pairing_acceptance(acceptance)
            .map(|_| ())
            .map_err(|_| PairingError::InvalidMembership)
    })
    .await
    .map_err(|_| PairingError::Storage)?
}
fn wall_hlc() -> Hlc {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis().min(u128::from(u64::MAX)) as u64);
    Hlc::new(millis, 0)
}

fn secure_message(
    session: &mut Session,
    tag: u8,
    plaintext: &[u8],
) -> Result<Message, PairingError> {
    let (seq, ciphertext) = session.seal(tag, plaintext)?;
    Ok(Message::Secure {
        tag,
        seq,
        ciphertext,
    })
}
fn open_secure(
    session: &mut Session,
    expected_tag: u8,
    message: Message,
) -> Result<Vec<u8>, PairingError> {
    match message {
        Message::Secure {
            tag,
            seq,
            ciphertext,
        } if tag == expected_tag => session.open(tag, seq, &ciphertext),
        Message::Secure { .. } => Err(PairingError::ProtocolMismatch),
        _ => Err(PairingError::ProtocolMismatch),
    }
}

fn begin_offer(offer: &mut PairingOffer, now: Instant) -> Result<Instant, PairingError> {
    match offer.state {
        OfferState::Cancelled => return Err(PairingError::Cancelled),
        OfferState::Consumed => return Err(PairingError::Unavailable),
        OfferState::LockedOut => return Err(PairingError::AttemptsExceeded),
        OfferState::InProgress => return Err(PairingError::Unavailable),
        OfferState::Expired => return Err(PairingError::Expired),
        OfferState::Available => {}
    }
    if now >= offer.expires_at {
        offer.state = OfferState::Expired;
        clear_offer_secrets(offer);
        return Err(PairingError::Expired);
    }
    offer.state = OfferState::InProgress;
    Ok(std::cmp::min(
        offer.expires_at,
        now + crate::transport::HANDSHAKE_TIMEOUT,
    ))
}
fn finish_offer(
    offer: &mut PairingOffer,
    result: &Result<PairingOutcome, PairingError>,
    now: Instant,
) {
    match result {
        Ok(_) => {
            offer.state = OfferState::Consumed;
            clear_offer_secrets(offer);
        }
        Err(PairingError::AuthenticationFailed | PairingError::InvalidPeerIdentity) => {
            offer.attempts = offer.attempts.saturating_add(1);
            offer.state = if offer.attempts >= MAX_FAILED_ATTEMPTS {
                OfferState::LockedOut
            } else if now >= offer.expires_at {
                OfferState::Expired
            } else {
                OfferState::Available
            };
            if !matches!(offer.state, OfferState::Available) {
                clear_offer_secrets(offer);
            }
        }
        Err(PairingError::Cancelled) => offer.state = OfferState::Cancelled,
        Err(PairingError::Expired) => {
            offer.state = OfferState::Expired;
            clear_offer_secrets(offer);
        }
        Err(_) => {
            offer.state = if now >= offer.expires_at {
                OfferState::Expired
            } else {
                OfferState::Available
            }
        }
    }
}

async fn inviter_inner<S>(
    mut io: FramedIo<S>,
    offer: &PairingOffer,
    store: PairingStoreHandle,
) -> Result<PairingOutcome, PairingError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let hello = receive(&mut io).await?;
    match hello {
        Message::ClientHello {
            instance_id,
            protocol_version,
        } if instance_id == offer.instance_id && protocol_version == VERSION => {}
        Message::ClientHello { instance_id, .. } if instance_id != offer.instance_id => {
            return Err(PairingError::Unavailable);
        }
        Message::ClientHello { .. } => return Err(PairingError::ProtocolMismatch),
        _ => return Err(PairingError::ProtocolMismatch),
    }
    let first = match receive(&mut io).await {
        Ok(Message::SpakeA(bytes)) => bytes,
        Ok(_) | Err(_) => return Err(PairingError::AuthenticationFailed),
    };
    let (spake, second) = start_b(&offer.secret, offer.instance_id);
    send(&mut io, &Message::SpakeB(second.clone())).await?;
    let shared = Zeroizing::new(
        spake
            .finish(&first)
            .map_err(|_| PairingError::AuthenticationFailed)?,
    );
    let transcript_hash = transcript(offer.instance_id, &first, &second);
    let keys = derive_keys(offer.instance_id, transcript_hash, &shared)?;
    let mut session = Session {
        instance: offer.instance_id,
        role: Role::B,
        keys,
        send: 0,
        recv: 0,
    };
    let confirm = match receive(&mut io).await {
        Ok(Message::ConfirmA(tag)) => tag,
        Ok(_) | Err(_) => return Err(PairingError::AuthenticationFailed),
    };
    verify_confirmation(
        session.keys.confirm_a.as_ref(),
        &transcript_hash,
        b'A',
        &confirm,
    )?;
    send(
        &mut io,
        &Message::ConfirmB(confirmation(
            session.keys.confirm_b.as_ref(),
            &transcript_hash,
            b'B',
        )),
    )
    .await?;

    let identity_message = receive(&mut io)
        .await
        .map_err(|_| PairingError::InvalidPeerIdentity)?;
    let identity = decode_identity(
        &open_secure(&mut session, T_IDENTITY, identity_message)
            .map_err(|_| PairingError::InvalidPeerIdentity)?,
    )?;
    let package = prepare_offer(&store, identity.clone()).await?;
    let encoded = encode_records(&package.records)?;
    let header = Zeroizing::new(encode_header(
        &package,
        package.records.len(),
        encoded.len(),
    )?);
    send(&mut io, &secure_message(&mut session, T_HEADER, &header)?).await?;
    for (index, chunk) in encoded.chunks(CHUNK_BYTES).enumerate() {
        let mut body = Vec::with_capacity(4 + chunk.len());
        body.extend_from_slice(&(index as u32).to_le_bytes());
        body.extend_from_slice(chunk);
        send(&mut io, &secure_message(&mut session, T_CHUNK, &body)?).await?;
    }
    let acceptance = decode_acceptance(&open_secure(
        &mut session,
        T_ACCEPTANCE,
        receive(&mut io).await?,
    )?)?;
    if acceptance.vault_id != package.vault_id
        || acceptance.admission_hash != package.admission_hash
        || acceptance.admitted_device_id != identity.device_id
    {
        return Err(PairingError::InvalidMembership);
    }
    finalize_acceptance(&store, acceptance).await?;
    let final_body = package.admission_hash.as_ref().to_vec();
    send(
        &mut io,
        &secure_message(&mut session, T_FINAL, &final_body)?,
    )
    .await?;
    Ok(PairingOutcome {
        vault_id: package.vault_id,
        inviter_device_id: package.inviter_device_id,
        admitted_device_id: identity.device_id,
        admission_hash: package.admission_hash,
    })
}

/// Run one inviter attempt.  The offer owns the secret and state, so dropping
/// this future cannot leave a background task holding sensitive material.
pub async fn run_inviter<S>(
    io: FramedIo<S>,
    offer: &mut PairingOffer,
    store: PairingStoreHandle,
    now: impl Fn() -> Instant,
) -> Result<PairingOutcome, PairingError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let deadline = begin_offer(offer, now())?;
    let result = match time::timeout_at(deadline, inviter_inner(io, offer, store)).await {
        Ok(result) => result,
        Err(_) => Err(PairingError::Expired),
    };
    finish_offer(offer, &result, now());
    result
}

async fn joiner_inner<S>(
    mut io: FramedIo<S>,
    target: PairingTarget,
    request: JoinRequest,
) -> Result<(Vault, PairingOutcome), PairingError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    if request.code.locator != locator_for(target.instance_id) {
        return Err(PairingError::InvalidInput);
    }
    let prepared = Vault::prepare_joining_device(&request.display_name, VERSION)
        .map_err(|_| PairingError::Storage)?;
    let identity = prepared.identity().clone();
    let (spake, first) = start_a(&request.code.secret, target.instance_id);
    send(
        &mut io,
        &Message::ClientHello {
            instance_id: target.instance_id,
            protocol_version: VERSION,
        },
    )
    .await?;
    send(&mut io, &Message::SpakeA(first.clone())).await?;
    let second = match receive(&mut io).await? {
        Message::SpakeB(bytes) => bytes,
        _ => return Err(PairingError::AuthenticationFailed),
    };
    let shared = Zeroizing::new(
        spake
            .finish(&second)
            .map_err(|_| PairingError::AuthenticationFailed)?,
    );
    let transcript_hash = transcript(target.instance_id, &first, &second);
    let keys = derive_keys(target.instance_id, transcript_hash, &shared)?;
    let mut session = Session {
        instance: target.instance_id,
        role: Role::A,
        keys,
        send: 0,
        recv: 0,
    };
    send(
        &mut io,
        &Message::ConfirmA(confirmation(
            session.keys.confirm_a.as_ref(),
            &transcript_hash,
            b'A',
        )),
    )
    .await?;
    let confirm = match receive(&mut io).await? {
        Message::ConfirmB(tag) => tag,
        _ => return Err(PairingError::AuthenticationFailed),
    };
    verify_confirmation(
        session.keys.confirm_b.as_ref(),
        &transcript_hash,
        b'B',
        &confirm,
    )?;
    let identity_bytes = encode_identity(&identity)?;
    send(
        &mut io,
        &secure_message(&mut session, T_IDENTITY, &identity_bytes)?,
    )
    .await?;

    let header_plain = Zeroizing::new(open_secure(
        &mut session,
        T_HEADER,
        receive(&mut io).await?,
    )?);
    let header = decode_header(&header_plain)?;
    let TransferHeader {
        vault_id,
        admission_hash,
        inviter_device_id,
        dek,
        record_count: count,
        total_bytes: total,
    } = header;
    let mut transfer = Vec::with_capacity(total);
    let mut expected_chunk = 0_u32;
    while transfer.len() < total {
        let body = open_secure(&mut session, T_CHUNK, receive(&mut io).await?)?;
        if body.len() < 4 {
            return Err(PairingError::InvalidMembership);
        }
        let index = u32::from_le_bytes(
            body[..4]
                .try_into()
                .map_err(|_| PairingError::InvalidMembership)?,
        );
        if index != expected_chunk {
            return Err(PairingError::ReplayOrOrdering);
        }
        let bytes = &body[4..];
        if bytes.is_empty()
            || bytes.len() > CHUNK_BYTES
            || transfer
                .len()
                .checked_add(bytes.len())
                .ok_or(PairingError::TransferTooLarge)?
                > total
        {
            return Err(PairingError::TransferTooLarge);
        }
        transfer.extend_from_slice(bytes);
        expected_chunk = expected_chunk
            .checked_add(1)
            .ok_or(PairingError::TransferTooLarge)?;
    }
    if transfer.len() != total {
        return Err(PairingError::InvalidMembership);
    }
    let records = decode_records(&transfer, count)?;
    if !records
        .iter()
        .any(|r| r.record_hash().ok() == Some(admission_hash))
    {
        return Err(PairingError::InvalidMembership);
    }
    let package = PairingVaultPackage {
        vault_id,
        dek: locker_core::Dek::from_bytes(*dek),
        records,
        admission_hash,
        inviter_device_id,
    };
    let destination = request.destination;
    let password = request.master_password;
    let accepted_at = wall_hlc();
    let (vault, acceptance) = crate::spawn_core_blocking(move || {
        Vault::create_from_pairing(
            &destination,
            password.as_bytes(),
            prepared,
            package,
            accepted_at,
        )
        .map_err(|_| PairingError::Storage)
    })
    .await
    .map_err(|_| PairingError::Storage)??;
    send(
        &mut io,
        &secure_message(&mut session, T_ACCEPTANCE, &encode_acceptance(&acceptance)?)?,
    )
    .await?;
    let final_hash = open_secure(&mut session, T_FINAL, receive(&mut io).await?)?;
    if final_hash.as_slice() != admission_hash.as_ref() {
        return Err(PairingError::AuthenticationFailed);
    }
    Ok((
        vault,
        PairingOutcome {
            vault_id,
            inviter_device_id,
            admitted_device_id: identity.device_id,
            admission_hash,
        },
    ))
}

/// Join an inviter using a human-entered code.  The core transaction is the
/// only blocking operation and runs off the Tokio worker pool.
pub async fn run_joiner<S>(
    io: FramedIo<S>,
    target: PairingTarget,
    request: JoinRequest,
    now: impl Fn() -> Instant,
) -> Result<(Vault, PairingOutcome), PairingError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let deadline = now() + std::cmp::min(PAIRING_LIFETIME, crate::transport::HANDSHAKE_TIMEOUT);
    match time::timeout_at(deadline, joiner_inner(io, target, request)).await {
        Ok(result) => result,
        Err(_) => Err(PairingError::Expired),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tokio::io::duplex;

    #[test]
    fn code_round_trips_and_rejects_ambiguous_symbols() {
        let offer = create_offer(Instant::now()).unwrap();
        let input: PairingCodeInput = offer.display_code.as_str().parse().unwrap();
        assert_eq!(&input.locator, &locator_for(offer.instance_id));
        assert_eq!(input.secret.as_bytes(), offer.secret.as_bytes());
        let compact = offer
            .display_code
            .as_str()
            .replace('-', "")
            .to_ascii_lowercase();
        let parsed: PairingCodeInput = compact.parse().unwrap();
        assert_eq!(parsed, input);
        assert!("00000-OOOOO-00000".parse::<PairingCodeInput>().is_err());
        assert!(
            format!("{}-{}", &offer.display_code.as_str()[..5], "1234")
                .parse::<PairingCodeInput>()
                .is_err()
        );
    }

    fn hex_bytes(value: &str) -> Vec<u8> {
        assert_eq!(value.len() % 2, 0);
        value
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }

    #[test]
    fn transcript_key_schedule_nonce_and_aad_golden_vectors() {
        let instance =
            PairingInstanceId::from_bytes([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
        let hash = transcript(instance, &[1, 2, 3], &[4, 5]);
        let expected_hash: [u8; 32] =
            hex_bytes("2a6df87c9d94e9e8a9c5c2a461ad0bc751e110dd8377b6baa69ed8462149ed39")
                .try_into()
                .unwrap();
        assert_eq!(hash, expected_hash);

        let shared: Vec<u8> = (0xa0..=0xbf).collect();
        let keys = derive_keys(instance, hash, &shared).unwrap();
        assert_eq!(
            &keys.confirm_a[..],
            hex_bytes("c277efe3f39497a7dcde232b7bce3af3d28116436d1f091e864b6a064282f4db")
        );
        assert_eq!(
            &keys.confirm_b[..],
            hex_bytes("9199a340e5618ab8b4f8afca12e9de0d36e0ec5b6004fc16685b949a7c0ad797")
        );
        assert_eq!(
            &keys.a_to_b[..],
            hex_bytes("417a932def5af08a3297e6d1b891eab0332fdd68479eaccbbeb0ce771c4ac7d0")
        );
        assert_eq!(
            &keys.b_to_a[..],
            hex_bytes("fbd19ebd8501f9db40125014e80cb98d792d040396320348c354016b59cffaf4")
        );
        assert_eq!(
            &keys.nonce_a_to_b[..],
            hex_bytes("a70caa06ae64c7c0c99a1221d8afdebc")
        );
        assert_eq!(
            &keys.nonce_b_to_a[..],
            hex_bytes("9af709c4636fa268c8a22334f0ccb650")
        );

        let expected_nonce: [u8; 24] =
            hex_bytes("101112131415161718191a1b1c1d1e1f0807060504030201")
                .try_into()
                .unwrap();
        assert_eq!(
            nonce(
                &[
                    16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31
                ],
                0x0102_0304_0506_0708,
            ),
            expected_nonce
        );
        assert_eq!(
            aad(instance, 1, 0x0102_0304_0506_0708, 9),
            hex_bytes(
                "4c4f434b45522d504149522d41454144000100000102030405060708090a0b0c0d0e0f01080706050403020109"
            )
        );
    }

    #[test]
    fn wrong_secret_and_offer_lifecycle_fail_closed() {
        let now = Instant::now();
        let first = create_offer(now).unwrap();
        let second = create_offer(now).unwrap();
        let (spake_a, first_message) = start_a(&first.secret, first.instance_id);
        let (spake_b, second_message) = start_b(&second.secret, first.instance_id);
        let shared_a = spake_a.finish(&second_message).unwrap();
        let shared_b = spake_b.finish(&first_message).unwrap();
        assert_ne!(shared_a, shared_b);

        let mut expired = create_offer(now - PAIRING_LIFETIME).unwrap();
        assert_eq!(begin_offer(&mut expired, now), Err(PairingError::Expired));
        assert_eq!(expired.state, OfferState::Expired);

        let mut cancelled = create_offer(now).unwrap();
        cancel_offer(&mut cancelled);
        assert_eq!(
            begin_offer(&mut cancelled, now),
            Err(PairingError::Cancelled)
        );

        let mut locked = create_offer(now).unwrap();
        locked.state = OfferState::InProgress;
        locked.attempts = MAX_FAILED_ATTEMPTS - 1;
        finish_offer(&mut locked, &Err(PairingError::AuthenticationFailed), now);
        assert_eq!(locked.state, OfferState::LockedOut);
        assert_eq!(
            begin_offer(&mut locked, now),
            Err(PairingError::AttemptsExceeded)
        );
    }

    #[test]
    fn spake_key_schedule_and_aead_are_directional_and_ordered() {
        let offer = create_offer(Instant::now()).unwrap();
        let (a, msg_a) = start_a(&offer.secret, offer.instance_id);
        let (b, msg_b) = start_b(&offer.secret, offer.instance_id);
        let ka = Zeroizing::new(a.finish(&msg_b).unwrap());
        let kb = Zeroizing::new(b.finish(&msg_a).unwrap());
        let hash = transcript(offer.instance_id, &msg_a, &msg_b);
        let keys_a = derive_keys(offer.instance_id, hash, &ka).unwrap();
        let keys_b = derive_keys(offer.instance_id, hash, &kb).unwrap();
        assert_eq!(keys_a.a_to_b.as_ref(), keys_b.a_to_b.as_ref());
        let mut sa = Session {
            instance: offer.instance_id,
            role: Role::A,
            keys: keys_a,
            send: 0,
            recv: 0,
        };
        let mut sb = Session {
            instance: offer.instance_id,
            role: Role::B,
            keys: keys_b,
            send: 0,
            recv: 0,
        };
        let message = sa.seal(T_IDENTITY, b"hello").unwrap();
        let opened = sb.open(T_IDENTITY, message.0, &message.1).unwrap();
        assert_eq!(opened, b"hello");
        assert!(matches!(
            sb.open(T_IDENTITY, message.0, &message.1),
            Err(PairingError::ReplayOrOrdering)
        ));
        let mut tampered = sa.seal(T_IDENTITY, b"x").unwrap();
        tampered.1[0] ^= 1;
        assert!(matches!(
            sb.open(T_IDENTITY, tampered.0, &tampered.1),
            Err(PairingError::AuthenticationFailed | PairingError::ReplayOrOrdering)
        ));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn loopback_pairing_commits_both_memberships() {
        let dir = std::env::temp_dir().join(format!("locker-pairing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir(&dir).unwrap();
        let root = dir.join("inviter.db");
        let destination = dir.join("joined.db");
        let inviter = Vault::create(b"inviter-password", &root).unwrap();
        let store = PairingStoreHandle::new(inviter.open_pairing_store().unwrap());
        let mut offer = create_offer(Instant::now()).unwrap();
        let target = PairingTarget {
            instance_id: offer.instance_id,
            inviter_endpoint: "127.0.0.1:1".parse().unwrap(),
        };
        let request = JoinRequest {
            code: offer.display_code.as_str().parse().unwrap(),
            destination: destination.clone(),
            master_password: SecretBytes::new(b"joiner-password"),
            display_name: "Joined device".to_owned(),
        };
        let (left, right) = duplex(256 * 1024);
        let left = FramedIo::new(left, crate::transport::TransportLimits::v1()).unwrap();
        let right = FramedIo::new(right, crate::transport::TransportLimits::v1()).unwrap();
        let inviter_task = run_inviter(left, &mut offer, store, Instant::now);
        let joiner_task = run_joiner(right, target, request, Instant::now);
        let (inviter_result, joiner_result) = tokio::join!(inviter_task, joiner_task);
        let inviter_outcome = inviter_result.unwrap();
        let (joined, joiner_outcome) = joiner_result.unwrap();
        assert_eq!(inviter_outcome, joiner_outcome);
        assert!(
            joined
                .membership()
                .unwrap()
                .is_active(joiner_outcome.admitted_device_id)
        );
        assert!(
            inviter
                .membership()
                .unwrap()
                .is_active(joiner_outcome.admitted_device_id)
        );
        drop(joined);
        drop(inviter);
        fs::remove_dir_all(dir).unwrap();
    }
}
