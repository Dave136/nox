//! Vault-key wrapping and device identity key operations.

use super::{
    cipher::Nonce,
    secret::{DecryptedPayload, Dek, Kek, SecretKey},
};
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::fmt;

const WRAP_AAD: &[u8] = b"locker/dek-wrap/v1";
pub(crate) const PRIVATE_KEY_AAD: &[u8] = b"locker/local-device/v1";

/// Errors returned by key wrapping and identity-key operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyError {
    /// OS randomness failed while creating a key or nonce.
    Randomness,
    /// A wrapped key or signature failed authentication.
    Invalid,
    /// A supplied key or signature had the wrong length or encoding.
    InvalidEncoding,
}

impl fmt::Display for KeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Randomness => "randomness unavailable",
            Self::Invalid => "invalid key material",
            Self::InvalidEncoding => "invalid key encoding",
        })
    }
}

impl std::error::Error for KeyError {}

/// A DEK encrypted under a local KEK.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WrappedDek {
    /// Fresh nonce used for the wrap operation.
    pub nonce: Nonce,
    /// Wrapped 32-byte DEK plus its authentication tag.
    pub ciphertext: Vec<u8>,
}

/// Alias used by vault-header code that treats the DEK wrapper generically.
pub type WrappedKey = WrappedDek;

/// Wrap a DEK with XChaCha20-Poly1305 under the local KEK.
pub fn wrap_dek(kek: &Kek, dek: &Dek) -> Result<WrappedDek, KeyError> {
    let mut nonce = [0_u8; super::cipher::NONCE_LENGTH];
    getrandom::fill(&mut nonce).map_err(|_| KeyError::Randomness)?;
    let cipher =
        XChaCha20Poly1305::new_from_slice(kek.as_bytes()).map_err(|_| KeyError::Invalid)?;
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: dek.as_bytes(),
                aad: WRAP_AAD,
            },
        )
        .map_err(|_| KeyError::Invalid)?;
    Ok(WrappedDek {
        nonce: Nonce::from_bytes(nonce),
        ciphertext,
    })
}

/// Unwrap a DEK, returning the same generic error for wrong KEKs and tampering.
pub fn unwrap_dek(kek: &Kek, wrapped: &WrappedDek) -> Result<Dek, KeyError> {
    let cipher =
        XChaCha20Poly1305::new_from_slice(kek.as_bytes()).map_err(|_| KeyError::Invalid)?;
    let plaintext = cipher
        .decrypt(
            XNonce::from_slice(wrapped.nonce.as_bytes()),
            Payload {
                msg: &wrapped.ciphertext,
                aad: WRAP_AAD,
            },
        )
        .map_err(|_| KeyError::Invalid)?;
    SecretKey::try_from_slice(&plaintext).map_err(|_| KeyError::Invalid)
}

/// Seal private key material using a fixed associated-data domain and prefix the nonce.
pub(crate) fn seal_fixed_aad(
    key: &SecretKey,
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, KeyError> {
    let mut nonce = [0_u8; super::cipher::NONCE_LENGTH];
    getrandom::fill(&mut nonce).map_err(|_| KeyError::Randomness)?;
    let cipher =
        XChaCha20Poly1305::new_from_slice(key.as_bytes()).map_err(|_| KeyError::Invalid)?;
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| KeyError::Invalid)?;
    let mut output = Vec::with_capacity(nonce.len() + ciphertext.len());
    output.extend_from_slice(&nonce);
    output.extend_from_slice(&ciphertext);
    Ok(output)
}

/// Open nonce-prefixed private key material using a fixed associated-data domain.
pub(crate) fn open_fixed_aad(
    key: &SecretKey,
    aad: &[u8],
    sealed: &[u8],
) -> Result<DecryptedPayload, KeyError> {
    if sealed.len() < super::cipher::NONCE_LENGTH + 16 {
        return Err(KeyError::Invalid);
    }
    let nonce = XNonce::from_slice(&sealed[..super::cipher::NONCE_LENGTH]);
    let cipher =
        XChaCha20Poly1305::new_from_slice(key.as_bytes()).map_err(|_| KeyError::Invalid)?;
    let plaintext = cipher
        .decrypt(
            nonce,
            Payload {
                msg: &sealed[super::cipher::NONCE_LENGTH..],
                aad,
            },
        )
        .map_err(|_| KeyError::Invalid)?;
    Ok(DecryptedPayload::new(plaintext))
}

/// A generated Ed25519 signing identity.
pub struct Ed25519Keypair {
    signing_key: SigningKey,
}

impl Ed25519Keypair {
    /// Generate a signing key from the operating-system CSPRNG.
    pub fn generate() -> Result<Self, KeyError> {
        let mut bytes = [0_u8; 32];
        getrandom::fill(&mut bytes).map_err(|_| KeyError::Randomness)?;
        Ok(Self::from_private_bytes(bytes))
    }

    /// Restore a keypair from its serialized 32-byte private seed.
    #[must_use]
    pub fn from_private_bytes(bytes: [u8; 32]) -> Self {
        Self {
            signing_key: SigningKey::from_bytes(&bytes),
        }
    }

    /// Return the serialized private seed for an explicit secure-storage path.
    #[must_use]
    pub fn private_key_bytes(&self) -> [u8; 32] {
        self.signing_key.to_bytes()
    }

    /// Alias for [`Self::private_key_bytes`].
    #[must_use]
    pub fn to_bytes(&self) -> [u8; 32] {
        self.private_key_bytes()
    }

    /// Return the canonical Ed25519 public key.
    #[must_use]
    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.signing_key.verifying_key().to_bytes()
    }

    /// Return the typed Ed25519 verifying key.
    #[must_use]
    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing_key.verifying_key()
    }

    /// Sign a message with this identity.
    #[must_use]
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.signing_key.sign(message).to_bytes()
    }
}

impl fmt::Debug for Ed25519Keypair {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Ed25519Keypair(<redacted>)")
    }
}

/// Generate an Ed25519 identity keypair.
pub fn generate_ed25519() -> Result<Ed25519Keypair, KeyError> {
    Ed25519Keypair::generate()
}

/// Descriptive alias for callers that name the returned value a keypair.
pub use generate_ed25519 as generate_ed25519_keypair;

/// Sign a message using a RustCrypto Ed25519 signing key.
#[must_use]
pub fn sign(signing_key: &SigningKey, message: &[u8]) -> [u8; 64] {
    signing_key.sign(message).to_bytes()
}

/// Verify a signature and return a generic failure for all invalid inputs.
pub fn verify_signature(
    public_key: impl AsRef<[u8]>,
    message: &[u8],
    signature: impl AsRef<[u8]>,
) -> Result<(), KeyError> {
    let public_key: [u8; 32] = public_key
        .as_ref()
        .try_into()
        .map_err(|_| KeyError::InvalidEncoding)?;
    let public_key = VerifyingKey::from_bytes(&public_key).map_err(|_| KeyError::Invalid)?;
    let signature = Signature::from_slice(signature.as_ref()).map_err(|_| KeyError::Invalid)?;
    public_key
        .verify(message, &signature)
        .map_err(|_| KeyError::Invalid)
}

/// Boolean convenience wrapper around [`verify_signature`].
#[must_use]
pub fn verify(public_key: impl AsRef<[u8]>, message: &[u8], signature: impl AsRef<[u8]>) -> bool {
    verify_signature(public_key, message, signature).is_ok()
}

/// A static X25519 keypair for a future Noise transport.
pub struct X25519Keypair {
    private_key: SecretKey,
    public_key: [u8; 32],
}

impl X25519Keypair {
    /// Generate a static X25519 keypair.
    pub fn generate() -> Result<Self, KeyError> {
        let private_key = SecretKey::random().map_err(|_| KeyError::Randomness)?;
        Ok(Self::from_private_key(private_key))
    }

    /// Restore a keypair from serialized private key bytes.
    #[must_use]
    pub fn from_private_bytes(bytes: [u8; 32]) -> Self {
        Self::from_private_key(SecretKey::from_bytes(bytes))
    }

    fn from_private_key(private_key: SecretKey) -> Self {
        let public_key = x25519_dalek::x25519(
            *private_key.as_bytes(),
            x25519_dalek::X25519_BASEPOINT_BYTES,
        );
        Self {
            private_key,
            public_key,
        }
    }

    /// Return the serialized private key for explicit secure storage.
    #[must_use]
    pub fn private_key_bytes(&self) -> [u8; 32] {
        *self.private_key.as_bytes()
    }

    /// Alias for [`Self::private_key_bytes`].
    #[must_use]
    pub fn to_bytes(&self) -> [u8; 32] {
        self.private_key_bytes()
    }

    /// Return the public static key used by Noise.
    #[must_use]
    pub fn public_key(&self) -> [u8; 32] {
        self.public_key
    }

    /// Alias for [`Self::public_key`].
    #[must_use]
    pub fn public_key_bytes(&self) -> [u8; 32] {
        self.public_key()
    }
}

impl fmt::Debug for X25519Keypair {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("X25519Keypair(<redacted>)")
    }
}

/// Generate an X25519 Noise static keypair.
pub fn generate_x25519() -> Result<X25519Keypair, KeyError> {
    X25519Keypair::generate()
}

/// Descriptive alias for callers that name the returned value a keypair.
pub use generate_x25519 as generate_x25519_keypair;
