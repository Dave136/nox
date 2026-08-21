//! Core-owned, synchronous onboarding façades for the pairing protocol.

use crate::{
    DeviceId, Ed25519Keypair, MembershipAcceptance, MembershipError, MembershipRecord,
    MembershipRecordHash, VaultError, VaultId, X25519Keypair, membership, storage::Db,
};
use std::fmt;

/// A freshly generated joining device identity and its private keys.
pub struct PreparedJoiningDevice {
    pub(crate) ed25519: Ed25519Keypair,
    pub(crate) x25519: X25519Keypair,
    pub(crate) identity: membership::DeviceIdentity,
}

impl PreparedJoiningDevice {
    pub(crate) fn new(
        ed25519: Ed25519Keypair,
        x25519: X25519Keypair,
        identity: membership::DeviceIdentity,
    ) -> Self {
        Self {
            ed25519,
            x25519,
            identity,
        }
    }

    /// Return the signed public identity that will be admitted.
    #[must_use]
    pub fn identity(&self) -> &membership::DeviceIdentity {
        &self.identity
    }

    /// Return the device identifier derived from the signing key.
    #[must_use]
    pub fn device_id(&self) -> DeviceId {
        self.identity.device_id
    }
}

impl fmt::Debug for PreparedJoiningDevice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PreparedJoiningDevice(<redacted>)")
    }
}

/// The inviter's authenticated package for a joining device.
pub struct PairingVaultPackage {
    pub vault_id: VaultId,
    pub dek: crate::Dek,
    pub records: Vec<MembershipRecord>,
    pub admission_hash: MembershipRecordHash,
    pub inviter_device_id: DeviceId,
}

impl fmt::Debug for PairingVaultPackage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairingVaultPackage")
            .field("vault_id", &self.vault_id)
            .field("dek", &"<redacted>")
            .field("records", &self.records.len())
            .field("admission_hash", &self.admission_hash)
            .field("inviter_device_id", &self.inviter_device_id)
            .finish()
    }
}

/// A separate synchronous store used by pairing jobs.
pub struct PairingStore {
    pub(crate) db: Db,
    pub(crate) vault_id: VaultId,
    pub(crate) device_id: DeviceId,
    pub(crate) dek: crate::Dek,
    pub(crate) ed25519: Ed25519Keypair,
}

impl fmt::Debug for PairingStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PairingStore(<redacted>)")
    }
}

impl PairingStore {
    pub(crate) fn new(
        db: Db,
        vault_id: VaultId,
        device_id: DeviceId,
        dek: crate::Dek,
        ed25519: Ed25519Keypair,
    ) -> Self {
        Self {
            db,
            vault_id,
            device_id,
            dek,
            ed25519,
        }
    }

    /// Persist an admission before returning the package sent to the joiner.
    pub fn prepare_pairing_offer(
        &mut self,
        joiner: membership::DeviceIdentity,
        admitted_at: crate::Hlc,
    ) -> Result<PairingVaultPackage, VaultError> {
        joiner.verify()?;
        let current = membership::load_membership(&self.db, self.vault_id)?;
        if !current.is_active(self.device_id) {
            return Err(VaultError::Membership(MembershipError::InvalidRecord(
                "inviter is not active",
            )));
        }
        if membership::load_authorization(&self.db, self.vault_id)?
            .is_locally_blocked(joiner.device_id)
        {
            return Err(VaultError::Membership(MembershipError::InvalidIdentity(
                "device is locally blocked",
            )));
        }
        if current.is_active(joiner.device_id) {
            return Err(VaultError::Membership(MembershipError::InvalidIdentity(
                "device is already active",
            )));
        }

        let existing = current.records().find_map(|record| match record {
            MembershipRecord::Admission(value)
                if value.admitted_device == joiner && value.invited_by == self.device_id =>
            {
                Some(record.clone())
            }
            _ => None,
        });
        let admission = if let Some(existing) = existing {
            existing
        } else {
            let admission = membership::create_admission(
                self.vault_id,
                joiner,
                self.device_id,
                admitted_at,
                &self.ed25519,
            )?;
            membership::insert_membership_record(&mut self.db, &admission)?;
            admission
        };
        let admission_hash = admission.record_hash()?;
        let validated = membership::load_membership(&self.db, self.vault_id)?;
        Ok(PairingVaultPackage {
            vault_id: self.vault_id,
            dek: self.dek.clone(),
            records: validated.records().cloned().collect(),
            admission_hash,
            inviter_device_id: self.device_id,
        })
    }

    /// Verify and persist the joiner's signed acceptance.
    pub fn finalize_pairing_acceptance(
        &mut self,
        acceptance: MembershipAcceptance,
    ) -> Result<membership::ValidatedMembership, VaultError> {
        if acceptance.vault_id != self.vault_id {
            return Err(VaultError::Membership(MembershipError::WrongVault));
        }
        let record = MembershipRecord::Acceptance(acceptance);
        self.db.transaction(|tx| {
            let current = membership::load_membership_tx(tx, self.vault_id)?;
            let record_hash = record.record_hash()?;
            if let Some(existing) = current.record(record_hash) {
                if existing == &record {
                    return Ok(current);
                }
                return Err(VaultError::Membership(MembershipError::ConflictingRecord));
            }

            let mut candidate_records: Vec<_> = current.records().cloned().collect();
            candidate_records.push(record.clone());
            membership::validate_membership(self.vault_id, &candidate_records)?;
            membership::insert_membership_record_tx(tx, &record)?;
            membership::load_membership_tx(tx, self.vault_id).map_err(VaultError::from)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Hlc, Vault};
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn blocked_joiner_cannot_receive_a_fresh_pairing_offer() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("locker-pairing-blocked-{unique}"));
        let path = directory.join("vault.db");
        let mut vault = Vault::create(b"password", &path).unwrap();
        let prepared = Vault::prepare_joining_device("joiner", 1).unwrap();
        let identity = prepared.identity().clone();

        let mut store = vault.open_pairing_store().unwrap();
        store
            .prepare_pairing_offer(identity.clone(), Hlc::new(1, 0))
            .unwrap();
        drop(store);

        vault
            .block_member(identity.device_id, Hlc::new(2, 0))
            .unwrap();
        let mut store = vault.open_pairing_store().unwrap();
        assert!(matches!(
            store.prepare_pairing_offer(identity, Hlc::new(3, 0)),
            Err(VaultError::Membership(MembershipError::InvalidIdentity(
                "device is locally blocked"
            )))
        ));

        drop(store);
        drop(vault);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn pairing_creation_preserves_an_existing_destination() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("locker-pairing-existing-{unique}"));
        let inviter_path = directory.join("inviter.db");
        let destination = directory.join("existing.db");
        let inviter = Vault::create(b"password", &inviter_path).unwrap();
        let prepared = Vault::prepare_joining_device("joiner", 1).unwrap();
        let identity = prepared.identity().clone();
        let mut store = inviter.open_pairing_store().unwrap();
        let package = store
            .prepare_pairing_offer(identity, Hlc::new(1, 0))
            .unwrap();
        drop(store);
        let original = b"not a vault";
        fs::create_dir_all(&directory).unwrap();
        fs::write(&destination, original).unwrap();

        assert!(matches!(
            Vault::create_from_pairing(
                &destination,
                b"joiner-password",
                prepared,
                package,
                Hlc::new(2, 0)
            ),
            Err(VaultError::VaultAlreadyExists)
        ));
        assert_eq!(fs::read(&destination).unwrap(), original);

        drop(inviter);
        fs::remove_dir_all(directory).unwrap();
    }
}
