//! Strongly typed identifiers used by the vault and change journal.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fmt, str::FromStr};
use ulid::Ulid;

/// A randomly generated vault identifier.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct VaultId([u8; 16]);

/// An identifier derived from an Ed25519 public key.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct DeviceId([u8; 32]);

/// A locally generated identifier for an encrypted item.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ItemId([u8; 16]);

/// A locally generated identifier for an immutable journal change.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ChangeId([u8; 16]);

/// Input accepted by [`DeviceId::from_public_key`].
pub trait PublicKeyBytes {
    /// Borrow the canonical public-key bytes.
    fn public_key_bytes(&self) -> &[u8];
}

impl<const N: usize> PublicKeyBytes for [u8; N] {
    fn public_key_bytes(&self) -> &[u8] {
        self
    }
}

impl PublicKeyBytes for [u8] {
    fn public_key_bytes(&self) -> &[u8] {
        self
    }
}

impl<T: PublicKeyBytes + ?Sized> PublicKeyBytes for &T {
    fn public_key_bytes(&self) -> &[u8] {
        (*self).public_key_bytes()
    }
}

impl PublicKeyBytes for ed25519_dalek::VerifyingKey {
    fn public_key_bytes(&self) -> &[u8] {
        self.as_bytes()
    }
}

/// The only validation error needed when accepting a raw public-key slice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublicKeyLengthError {
    /// Number of bytes supplied by the caller.
    pub actual: usize,
}

impl fmt::Display for PublicKeyLengthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "expected a 32-byte Ed25519 public key, got {} bytes",
            self.actual
        )
    }
}

impl std::error::Error for PublicKeyLengthError {}

impl VaultId {
    /// Generate a vault identifier using the operating-system CSPRNG.
    #[must_use]
    pub fn new() -> Self {
        Self::try_new().expect("operating-system randomness unavailable")
    }

    /// Generate a vault identifier without converting a randomness failure to a panic.
    pub fn try_new() -> Result<Self, getrandom::Error> {
        let mut bytes = [0_u8; 16];
        getrandom::fill(&mut bytes)?;
        Ok(Self(bytes))
    }

    /// Construct an identifier from its canonical bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Return the canonical bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

impl Default for VaultId {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceId {
    /// Derive a stable identifier from canonical public-key bytes.
    ///
    /// Callers that receive an untyped slice should use
    /// [`Self::try_from_public_key`] first. Hashing the supplied bytes here keeps
    /// this primitive independent from the keypair implementation in Task 2.
    #[must_use]
    pub fn from_public_key(public_key: impl PublicKeyBytes) -> Self {
        Self(Sha256::digest(public_key.public_key_bytes()).into())
    }

    /// Derive a device identifier after checking the Ed25519 key length.
    pub fn try_from_public_key(
        public_key: impl PublicKeyBytes,
    ) -> Result<Self, PublicKeyLengthError> {
        let bytes = public_key.public_key_bytes();
        if bytes.len() != 32 {
            return Err(PublicKeyLengthError {
                actual: bytes.len(),
            });
        }
        Ok(Self::from_public_key(bytes))
    }

    /// Derive an identifier from an Ed25519 verifying key without making the
    /// core identifier module own keypair construction.
    #[must_use]
    pub fn from_verifying_key(public_key: &ed25519_dalek::VerifyingKey) -> Self {
        Self::from_public_key(public_key)
    }

    /// Construct an identifier from its canonical bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Return the canonical bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl ItemId {
    /// Generate a ULID-backed item identifier.
    #[must_use]
    pub fn new() -> Self {
        Self(Ulid::new().to_bytes())
    }

    /// Construct an identifier from its canonical bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Return the canonical bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// View this identifier as a ULID.
    #[must_use]
    pub const fn as_ulid(&self) -> Ulid {
        Ulid::from_bytes(self.0)
    }
}

impl Default for ItemId {
    fn default() -> Self {
        Self::new()
    }
}

impl ChangeId {
    /// Generate a ULID-backed change identifier.
    #[must_use]
    pub fn new() -> Self {
        Self(Ulid::new().to_bytes())
    }

    /// Construct an identifier from its canonical bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Return the canonical bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }

    /// View this identifier as a ULID.
    #[must_use]
    pub const fn as_ulid(&self) -> Ulid {
        Ulid::from_bytes(self.0)
    }
}

impl Default for ChangeId {
    fn default() -> Self {
        Self::new()
    }
}

macro_rules! as_ref_bytes {
    ($($type:ty),+ $(,)?) => {
        $(
            impl AsRef<[u8]> for $type {
                fn as_ref(&self) -> &[u8] {
                    &self.0
                }
            }
        )+
    };
}

as_ref_bytes!(VaultId, DeviceId, ItemId, ChangeId);

macro_rules! hex_format {
    ($type:ty, $len:expr) => {
        impl fmt::Display for $type {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                for byte in self.0 {
                    write!(f, "{byte:02x}")?;
                }
                Ok(())
            }
        }

        impl FromStr for $type {
            type Err = &'static str;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                if value.len() != $len * 2 {
                    return Err("invalid identifier length");
                }
                let mut bytes = [0_u8; $len];
                for (index, byte) in bytes.iter_mut().enumerate() {
                    *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
                        .map_err(|_| "invalid identifier hex")?;
                }
                Ok(Self(bytes))
            }
        }
    };
}

hex_format!(VaultId, 16);
hex_format!(DeviceId, 32);

macro_rules! ulid_format {
    ($type:ty) => {
        impl fmt::Display for $type {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.as_ulid().fmt(f)
            }
        }

        impl FromStr for $type {
            type Err = ulid::DecodeError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Ok(Self(Ulid::from_string(value)?.to_bytes()))
            }
        }
    };
}

ulid_format!(ItemId);
ulid_format!(ChangeId);
