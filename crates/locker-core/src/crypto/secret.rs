//! Secret-owning buffers that redact diagnostics and zeroize on drop.

use std::fmt;
use zeroize::Zeroizing;

/// A variable-length secret such as a password or decrypted payload.
pub struct SecretBytes(Zeroizing<Vec<u8>>);

impl SecretBytes {
    /// Take ownership of secret bytes.
    #[must_use]
    pub fn new(bytes: impl AsRef<[u8]>) -> Self {
        Self(Zeroizing::new(bytes.as_ref().to_vec()))
    }

    /// Copy a borrowed secret into a zeroizing owner.
    #[must_use]
    pub fn from_slice(bytes: &[u8]) -> Self {
        Self::new(bytes)
    }

    /// Borrow the secret for a cryptographic operation.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Alias emphasizing that this is an intentional secret exposure.
    #[must_use]
    pub fn expose_secret(&self) -> &[u8] {
        self.as_bytes()
    }

    /// Return the secret length without exposing its contents.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the secret contains no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Copy the bytes out for an API that requires an owned buffer.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.0.to_vec()
    }
}

impl Clone for SecretBytes {
    fn clone(&self) -> Self {
        Self::from_slice(self.as_bytes())
    }
}

impl AsRef<[u8]> for SecretBytes {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretBytes(<redacted>)")
    }
}

/// A fixed-size zeroizing key buffer.
pub struct SecretKey(Zeroizing<[u8; 32]>);

impl SecretKey {
    /// Construct a key from exactly 32 bytes.
    #[must_use]
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// Construct a key from a checked byte slice.
    pub fn try_from_slice(bytes: &[u8]) -> Result<Self, SecretLengthError> {
        let bytes: [u8; 32] = bytes.try_into().map_err(|_| SecretLengthError {
            expected: 32,
            actual: bytes.len(),
        })?;
        Ok(Self::from_bytes(bytes))
    }

    /// Generate a key from the operating-system CSPRNG.
    pub fn random() -> Result<Self, getrandom::Error> {
        let mut bytes = [0_u8; 32];
        getrandom::fill(&mut bytes)?;
        Ok(Self::from_bytes(bytes))
    }

    /// Alias for [`Self::random`].
    pub fn generate() -> Result<Self, getrandom::Error> {
        Self::random()
    }

    /// Borrow the key for a cryptographic operation.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Alias emphasizing that this is an intentional key exposure.
    #[must_use]
    pub fn expose_secret(&self) -> &[u8; 32] {
        self.as_bytes()
    }

    /// Copy the key into an owned byte array.
    #[must_use]
    pub fn into_bytes(self) -> [u8; 32] {
        *self.0
    }
}

impl Clone for SecretKey {
    fn clone(&self) -> Self {
        Self::from_bytes(*self.as_bytes())
    }
}

impl AsRef<[u8]> for SecretKey {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SecretKey(<redacted>)")
    }
}

/// Invalid fixed-size secret input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SecretLengthError {
    /// Required byte count.
    pub expected: usize,
    /// Supplied byte count.
    pub actual: usize,
}

impl fmt::Display for SecretLengthError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "expected {} secret bytes, got {}",
            self.expected, self.actual
        )
    }
}

impl std::error::Error for SecretLengthError {}

/// Password material owned by the caller or KDF wrapper.
pub type Password = SecretBytes;
/// Key-encryption key.
pub type Kek = SecretKey;
/// Data-encryption key.
pub type Dek = SecretKey;
/// Noise/session key material.
pub type SessionKey = SecretKey;
/// Decrypted item payload.
pub type DecryptedPayload = SecretBytes;
/// Generic secret-buffer alias.
pub type Secret = SecretBytes;
