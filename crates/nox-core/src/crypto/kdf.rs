//! Argon2id key derivation for local vault passwords.

use super::secret::{Kek, SecretKey};
use argon2::{Algorithm, Argon2, Params, Version};
use serde::{Deserialize, Serialize};
use std::{borrow::Borrow, fmt};
use zeroize::Zeroizing;

/// Fixed v1 Argon2id memory cost: 64 MiB.
pub const DEFAULT_MEMORY_KIB: u32 = 64 * 1024;
/// Fixed v1 Argon2id pass count.
pub const DEFAULT_ITERATIONS: u32 = 3;
/// Fixed v1 Argon2id lane count.
pub const DEFAULT_PARALLELISM: u32 = 4;
/// Alias matching the vault-header field name.
pub const ARGON2_MEMORY_KIB: u32 = DEFAULT_MEMORY_KIB;
/// Alias matching the vault-header field name.
pub const ARGON2_ITERATIONS: u32 = DEFAULT_ITERATIONS;
/// Alias matching the vault-header field name.
pub const ARGON2_PARALLELISM: u32 = DEFAULT_PARALLELISM;
/// Derived KEK size in bytes.
pub const KEK_LENGTH: usize = 32;
/// Vault-header salt size in bytes.
pub const SALT_LENGTH: usize = 16;

/// Persisted Argon2id parameters. The fields map directly to vault-header
/// `argon2_memory_kib`, `argon2_iterations`, and `argon2_parallelism`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Argon2Params {
    /// Memory cost in KiB.
    pub memory_kib: u32,
    /// Number of Argon2 passes.
    pub iterations: u32,
    /// Number of lanes.
    pub parallelism: u32,
}

impl Argon2Params {
    /// Construct explicit parameters, validated when used for derivation.
    #[must_use]
    pub const fn new(memory_kib: u32, iterations: u32, parallelism: u32) -> Self {
        Self {
            memory_kib,
            iterations,
            parallelism,
        }
    }

    /// The conservative fixed v1 parameters.
    #[must_use]
    pub const fn v1() -> Self {
        Self::new(DEFAULT_MEMORY_KIB, DEFAULT_ITERATIONS, DEFAULT_PARALLELISM)
    }
}

impl Default for Argon2Params {
    fn default() -> Self {
        Self::v1()
    }
}

/// Descriptive alias used by vault-header code.
pub type KdfParams = Argon2Params;

/// Errors returned by Argon2id derivation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KdfError {
    /// The supplied salt is shorter than Argon2's minimum.
    SaltTooShort,
    /// The memory, pass, or lane parameters are invalid.
    InvalidParameters,
    /// Argon2 rejected the derivation request.
    DerivationFailed,
}

impl fmt::Display for KdfError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::SaltTooShort => "KDF salt is too short",
            Self::InvalidParameters => "invalid Argon2id parameters",
            Self::DerivationFailed => "Argon2id derivation failed",
        })
    }
}

impl std::error::Error for KdfError {}

/// Derive a 32-byte KEK from a password, salt, and stored Argon2id parameters.
pub fn derive_kek<P, S, R>(password: P, salt: S, params: R) -> Result<Kek, KdfError>
where
    P: AsRef<[u8]>,
    S: AsRef<[u8]>,
    R: Borrow<Argon2Params>,
{
    let salt = salt.as_ref();
    if salt.len() < argon2::MIN_SALT_LEN {
        return Err(KdfError::SaltTooShort);
    }

    let params = params.borrow();
    let params = Params::new(
        params.memory_kib,
        params.iterations,
        params.parallelism,
        Some(KEK_LENGTH),
    )
    .map_err(|_| KdfError::InvalidParameters)?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut output = Zeroizing::new([0_u8; KEK_LENGTH]);
    argon2
        .hash_password_into(password.as_ref(), salt, &mut *output)
        .map_err(|_| KdfError::DerivationFailed)?;
    Ok(SecretKey::from_bytes(*output))
}

/// Generate a fresh vault-header salt.
pub fn random_salt() -> Result<[u8; SALT_LENGTH], getrandom::Error> {
    let mut salt = [0_u8; SALT_LENGTH];
    getrandom::fill(&mut salt)?;
    Ok(salt)
}
