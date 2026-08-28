//! Core vault, storage, and cryptographic primitives for Nox.

pub mod backup;
pub mod clock;
pub mod crypto;
pub mod ids;
pub mod item;
pub mod journal;
pub mod membership;
pub mod merge;
pub mod pairing;
pub mod password;
pub mod storage;
pub mod vault;

pub use backup::{
    BackupError, BackupExportRequest, MAX_ARCHIVE_BYTES, MAX_BACKUP_PASSWORD_BYTES, RestoreResult,
    export, export_backup, export_to_path, restore, restore_backup, restore_from_path,
};
pub use clock::{
    Accept, ChangeOrderKey, ClockStamp, Hlc, HlcClock, HlcError, HlcOrderKey, HlcOrderingKey,
    HlcTimestamp, OrderingKey, Quarantine, SkewClassification, SkewDecision, classify,
};
pub use ids::{ChangeId, DeviceId, ItemId, PublicKeyBytes, PublicKeyLengthError, VaultId};
pub use item::{ITEM_SCHEMA_VERSION, ItemPayload, ItemPayloadError, ItemType};
pub use journal::{
    ApplyResult, BatchApplyResult, Change, CursorMap, JournalError, MAX_BATCH_PLAINTEXT_BYTES,
    MAX_CHANGE_CIPHERTEXT_BYTES, MAX_CHANGES_PER_BATCH, MAX_CURSOR_ENTRIES,
    MAX_ENCODED_CHANGE_BYTES, ReplicationStore, ReplicationStoreError, apply_received_change,
    create_local_change, decode_change, encode_change,
};
pub use membership::{
    AuthorizationSnapshot, DeviceIdentity, MAX_ACTIVE_MEMBERS, MAX_DEVICE_DISPLAY_NAME_BYTES,
    MAX_MEMBERSHIP_RECORD_BYTES, MAX_MEMBERSHIP_RECORDS, MEMBERSHIP_FORMAT_VERSION,
    MemberPublicKeys, MembershipAcceptance, MembershipAdmission, MembershipError,
    MembershipGenesis, MembershipInsert, MembershipRecord, MembershipRecordHash,
    ValidatedMembership, block_device, create_acceptance, create_admission, create_genesis,
    insert_membership_record, load_authorization, load_membership, unblock_device,
    validate_membership,
};
pub use merge::{MergeError, apply_merge_projection, resolve_conflicts, unresolved_heads};
pub use pairing::{PairingStore, PairingVaultPackage, PreparedJoiningDevice};
pub use password::{CharClasses, MAX_LENGTH, PasswordError, generate_password};

pub use crypto::cipher::{
    AeadContext, AssociatedData, ChangeAad, Ciphertext, EncryptedPayload, Nonce, Operation,
};
pub use crypto::kdf::{Argon2Params, KdfParams, derive_kek};
pub use crypto::keys::{
    Ed25519Keypair, WrappedDek, WrappedKey, X25519Keypair, unwrap_dek, wrap_dek,
};
pub use crypto::secret::{
    DecryptedPayload, Dek, Kek, Password, Secret, SecretBytes, SecretKey, SessionKey,
};
pub use vault::{UnlockedSyncAccess, Vault, VaultError, default_vault_path};

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn typed_ids_have_expected_generation_and_round_trip() {
        let vault = VaultId::new();
        assert_ne!(vault, VaultId::new());
        assert_eq!(vault, VaultId::from_bytes(*vault.as_bytes()));

        let item = ItemId::new();
        assert_eq!(item, ItemId::from_bytes(*item.as_bytes()));
        assert_ne!(item, ItemId::new());

        let change = ChangeId::new();
        assert_eq!(change, ChangeId::from_bytes(*change.as_bytes()));
        assert_ne!(change, ChangeId::new());
    }

    #[test]
    fn device_id_is_stable_for_the_same_raw_public_key() {
        let public_key = [7_u8; 32];
        assert_eq!(
            DeviceId::from_public_key(public_key),
            DeviceId::from_public_key(public_key)
        );
        assert_ne!(
            DeviceId::from_public_key(public_key),
            DeviceId::from_public_key([8_u8; 32])
        );

        let signing_key = ed25519_dalek::SigningKey::from_bytes(&public_key);
        let canonical_public_key = signing_key.verifying_key().to_bytes();
        assert_eq!(
            DeviceId::from_public_key(signing_key.verifying_key()),
            DeviceId::from_public_key(canonical_public_key)
        );
    }

    #[test]
    fn ordering_key_breaks_hlc_ties_by_device_then_origin_sequence() {
        let timestamp = Hlc::new(100, 2);
        let first = HlcOrderKey::new(timestamp, DeviceId::from_bytes([1; 32]), 9);
        let second = HlcOrderKey::new(timestamp, DeviceId::from_bytes([2; 32]), 1);
        let third = HlcOrderKey::new(timestamp, DeviceId::from_bytes([2; 32]), 2);

        assert!(first < second);
        assert!(second < third);
    }

    #[test]
    fn clock_advances_monotonically_and_persists_origin_sequence() {
        let mut clock = HlcClock::new();
        assert_eq!(clock.tick(1_000).unwrap(), Hlc::new(1_000, 0));
        assert_eq!(clock.origin_seq(), 1);
        assert_eq!(clock.tick(1_000).unwrap(), Hlc::new(1_000, 1));
        assert_eq!(clock.origin_seq(), 2);
        assert_eq!(clock.tick(999).unwrap(), Hlc::new(1_000, 2));
        assert_eq!(clock.origin_seq(), 3);
    }

    #[test]
    fn future_skew_is_accepted_at_the_boundary_and_quarantined_after_it() {
        let local = Hlc::new(1_000, 0);
        assert_eq!(classify(Hlc::new(1_300, 99), local, 300), Accept);
        assert_eq!(classify(Hlc::new(1_301, 0), local, 300), Quarantine);
        assert_eq!(classify(Hlc::new(999, 0), local, 0), Accept);
    }
}

#[cfg(test)]
mod task2_tests {
    use super::*;
    use crate::crypto::{cipher, keys};

    #[test]
    fn secret_debug_output_is_redacted() {
        let secret = SecretBytes::new(b"correct horse battery staple");
        let debug = format!("{secret:?}");

        assert!(!debug.contains("correct"));
        assert!(debug.contains("redacted"));
    }

    #[test]
    fn argon2id_derivation_is_deterministic_for_explicit_parameters() {
        let params = Argon2Params::new(32, 3, 4);
        let first = derive_kek(b"password", b"0123456789abcdef", params).unwrap();
        let second = derive_kek(b"password", b"0123456789abcdef", params).unwrap();
        let other = derive_kek(b"different", b"0123456789abcdef", params).unwrap();

        assert_eq!(
            first.as_bytes(),
            &[
                0x6d, 0x9b, 0x25, 0xa4, 0xaf, 0xed, 0xfa, 0x91, 0xcb, 0x0b, 0x84, 0xdf, 0x16, 0x2a,
                0xf3, 0x31, 0xe2, 0x0c, 0x8b, 0x23, 0x63, 0x1c, 0x03, 0xbf, 0xec, 0x92, 0x46, 0xad,
                0x4d, 0x88, 0xf2, 0xe1,
            ]
        );
        assert_eq!(first.as_bytes(), second.as_bytes());
        assert_ne!(first.as_bytes(), other.as_bytes());
    }

    #[test]
    fn xchacha_round_trip_rejects_tampering_and_aad_swaps() {
        let key = SecretKey::from_bytes([7; 32]);
        let context = AeadContext::new(
            VaultId::from_bytes([1; 16]),
            ItemId::from_bytes([2; 16]),
            ChangeId::from_bytes([3; 16]),
            vec![],
            DeviceId::from_bytes([4; 32]),
            1,
            Hlc::new(5, 0),
            Operation::Upsert,
            1,
        );
        let encrypted = cipher::encrypt(&key, &context, b"payload").unwrap();
        let decrypted = cipher::decrypt(&key, &context, &encrypted).unwrap();
        assert_eq!(decrypted.as_bytes(), b"payload");

        let mut tampered = encrypted.clone();
        tampered.ciphertext[0] ^= 1;
        assert!(cipher::decrypt(&key, &context, &tampered).is_err());

        let swapped = AeadContext {
            item_id: ItemId::from_bytes([9; 16]),
            ..context
        };
        assert!(cipher::decrypt(&key, &swapped, &encrypted).is_err());
    }

    #[test]
    fn xchacha_generates_a_fresh_nonce_for_each_encryption() {
        let key = SecretKey::from_bytes([8; 32]);
        let context = AeadContext::new(
            VaultId::from_bytes([1; 16]),
            ItemId::from_bytes([2; 16]),
            ChangeId::from_bytes([3; 16]),
            vec![],
            DeviceId::from_bytes([4; 32]),
            1,
            Hlc::new(5, 0),
            Operation::Tombstone,
            1,
        );
        let first = cipher::encrypt(&key, &context, []).unwrap();
        let second = cipher::encrypt(&key, &context, []).unwrap();
        assert_ne!(first.nonce, second.nonce);
        assert!(
            cipher::decrypt(
                &key,
                &context,
                &EncryptedPayload {
                    nonce: second.nonce,
                    ciphertext: first.ciphertext.clone(),
                }
            )
            .is_err()
        );
    }

    #[test]
    fn wrapping_signing_and_x25519_key_operations_fail_closed() {
        let kek = Kek::from_bytes([1; 32]);
        let dek = Dek::from_bytes([2; 32]);
        let wrapped = wrap_dek(&kek, &dek).unwrap();
        assert_eq!(
            unwrap_dek(&kek, &wrapped).unwrap().as_bytes(),
            dek.as_bytes()
        );
        assert!(unwrap_dek(&Kek::from_bytes([3; 32]), &wrapped).is_err());

        let signing = Ed25519Keypair::generate().unwrap();
        let message = b"signed change";
        let signature = signing.sign(message);
        assert!(keys::verify_signature(signing.public_key_bytes(), message, signature).is_ok());
        assert!(keys::verify_signature(signing.public_key_bytes(), b"altered", signature).is_err());

        let first = X25519Keypair::from_private_bytes([4; 32]);
        let second = X25519Keypair::from_private_bytes([5; 32]);
        assert!(X25519Keypair::generate().is_ok());
        assert_ne!(first.public_key(), second.public_key());
        assert_eq!(
            first.public_key(),
            X25519Keypair::from_private_bytes(first.private_key_bytes()).public_key()
        );
    }
}

#[cfg(test)]
mod task7_tests {
    use super::*;
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicUsize, Ordering},
    };

    static NEXT_BACKUP_TEST: AtomicUsize = AtomicUsize::new(0);

    fn path(label: &str) -> PathBuf {
        let id = NEXT_BACKUP_TEST.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("locker-task7-{label}-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        path
    }

    fn cleanup(path: &PathBuf) {
        let _ = fs::remove_file(path);
        for suffix in ["-wal", "-shm"] {
            let mut sidecar = path.as_os_str().to_os_string();
            sidecar.push(suffix);
            let _ = fs::remove_file(PathBuf::from(sidecar));
        }
        let temp = std::env::temp_dir();
        if path.parent() != Some(temp.as_path()) {
            let _ = fs::remove_dir_all(path.parent().unwrap_or(Path::new(".")));
        }
    }

    #[test]
    fn v2_backup_restores_with_a_new_master_password() {
        let source = path("source").join("vault.db");
        let archive = path("archive").with_extension("lockbak");
        let destination = path("destination").join("vault.db");
        let mut vault = Vault::create(b"old-master", &source).unwrap();
        vault
            .create_item(&ItemPayload {
                schema_version: ITEM_SCHEMA_VERSION,
                item_type: ItemType::Login,
                title: "backup marker".into(),
                username: "alice".into(),
                password: "secret".into(),
                uris: vec![],
                notes: String::new(),
                created_at: 1,
                updated_at: 1,
            })
            .unwrap();
        let request = vault
            .prepare_backup_export(b"backup-password", &archive)
            .unwrap();
        assert_eq!(format!("{request:?}"), "BackupExportRequest(<redacted>)");
        request.run().unwrap();
        vault.lock();
        let _result =
            restore_from_path(&archive, &destination, b"backup-password", b"new-master").unwrap();
        let restored = Vault::unlock(b"new-master", &destination).unwrap();
        assert!(
            restored
                .list_items()
                .unwrap()
                .iter()
                .any(|(_, payload)| payload.title == "backup marker")
        );
        assert!(matches!(
            Vault::unlock(b"old-master", &destination),
            Err(VaultError::IncorrectPasswordOrCorruptVault)
        ));
        cleanup(&source);
        cleanup(&archive);
        cleanup(&destination);
    }

    #[test]
    fn v2_wrong_password_is_authentication_failed() {
        let source = path("wrong-source").join("vault.db");
        let archive = path("wrong-archive").with_extension("lockbak");
        let destination = path("wrong-destination").join("vault.db");
        let vault = Vault::create(b"master", &source).unwrap();
        vault
            .prepare_backup_export(b"backup-password", &archive)
            .unwrap()
            .run()
            .unwrap();
        assert!(matches!(
            restore_from_path(&archive, &destination, b"wrong", b"new"),
            Err(BackupError::AuthenticationFailed)
        ));
        cleanup(&source);
        cleanup(&archive);
        cleanup(&destination);
    }

    #[test]
    fn v1_archive_is_reported_as_unsupported() {
        let archive = path("v1-archive").with_extension("lockbak");
        let destination = path("v1-destination").join("vault.db");
        let mut bytes = b"NOXBACK1".to_vec();
        bytes.resize(64, 0);
        fs::write(&archive, bytes).unwrap();
        assert!(matches!(
            restore_from_path(&archive, &destination, b"backup-password", b"new-master"),
            Err(BackupError::UnsupportedVersion)
        ));
        cleanup(&archive);
        cleanup(&destination);
    }

    #[test]
    fn v2_tampering_is_authenticated_before_destination_mutation() {
        let source = path("tamper-source").join("vault.db");
        let archive = path("tamper-archive").with_extension("lockbak");
        let destination = path("tamper-destination").join("vault.db");
        let mut source_vault = Vault::create(b"source-master", &source).unwrap();
        source_vault
            .create_item(&ItemPayload {
                schema_version: ITEM_SCHEMA_VERSION,
                item_type: ItemType::Login,
                title: "source".into(),
                username: "alice".into(),
                password: "secret".into(),
                uris: vec![],
                notes: String::new(),
                created_at: 1,
                updated_at: 1,
            })
            .unwrap();
        source_vault
            .prepare_backup_export(b"backup-password", &archive)
            .unwrap()
            .run()
            .unwrap();
        source_vault.lock();

        let mut destination_vault = Vault::create(b"destination-master", &destination).unwrap();
        destination_vault
            .create_item(&ItemPayload {
                schema_version: ITEM_SCHEMA_VERSION,
                item_type: ItemType::Login,
                title: "sentinel".into(),
                username: "keep".into(),
                password: "untouched".into(),
                uris: vec![],
                notes: String::new(),
                created_at: 1,
                updated_at: 1,
            })
            .unwrap();
        destination_vault.lock();

        let original = fs::read(&archive).unwrap();
        for index in [11, 65, original.len() - 1] {
            let mut tampered = original.clone();
            tampered[index] ^= 1;
            let tampered_path = path("tampered").with_extension(format!("{index}.lockbak"));
            fs::write(&tampered_path, tampered).unwrap();
            assert!(matches!(
                restore_from_path(
                    &tampered_path,
                    &destination,
                    b"backup-password",
                    b"new-master"
                ),
                Err(BackupError::AuthenticationFailed)
            ));
            assert!(destination.exists(), "destination disappeared at {index}");
            let _ = fs::remove_file(&tampered_path);
        }
        let unchanged = Vault::unlock(b"destination-master", &destination).unwrap();
        assert_eq!(unchanged.list_items().unwrap()[0].1.title, "sentinel");
        unchanged.lock();
        cleanup(&source);
        cleanup(&archive);
        cleanup(&destination);
    }

    #[test]
    fn file_backed_databases_use_wal() {
        let path = path("wal").join("vault.db");
        let db = storage::db::Db::open(&path).unwrap();
        let mode: String = db
            .connection()
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(mode.to_ascii_lowercase(), "wal");
        cleanup(&path);
    }
}

#[cfg(test)]
mod task3_tests {
    use super::storage::{db::Db, schema::TABLE_NAMES};
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn migration_creates_every_required_table_and_is_idempotent() {
        let mut db = Db::open_in_memory().unwrap();
        let expected = TABLE_NAMES
            .iter()
            .map(|name| (*name).to_owned())
            .collect::<std::collections::BTreeSet<_>>();
        let actual = db
            .connection()
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .filter(|name| name != "sqlite_sequence")
            .collect::<std::collections::BTreeSet<_>>();

        assert_eq!(actual, expected);
        assert_eq!(db.user_version().unwrap(), 1);
        db.migrate().unwrap();
        assert_eq!(db.user_version().unwrap(), 1);
    }

    #[test]
    fn transaction_rolls_back_when_the_callback_fails() {
        let mut db = Db::open_in_memory().unwrap();
        let result = db.transaction(|tx| -> rusqlite::Result<()> {
            tx.execute(
                "INSERT INTO blocked_devices (device_id, blocked_at_physical_ms, blocked_at_logical) VALUES (?1, ?2, ?3)",
                (vec![1_u8; 32], 10_i64, 0_i64),
            )?;
            Err(rusqlite::Error::InvalidQuery)
        });

        assert!(result.is_err());
        let count: i64 = db
            .connection()
            .query_row("SELECT count(*) FROM blocked_devices", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[cfg(unix)]
    #[test]
    fn file_database_and_parent_directory_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!("locker-task3-{unique}"));
        let path = directory.join("vault.db");
        let _db = Db::open(&path).unwrap();

        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        drop(_db);
        fs::remove_dir_all(directory).unwrap();
    }
}

#[cfg(test)]
mod task4_tests {
    use super::{
        DeviceId, Ed25519Keypair, Hlc, ItemId, JournalError, Operation, SecretKey, VaultId,
        apply_received_change, create_local_change, crypto::cipher, storage::Db,
    };

    fn fixture(private_byte: u8) -> (VaultId, ItemId, Ed25519Keypair, DeviceId, SecretKey) {
        let signing_key = Ed25519Keypair::from_private_bytes([private_byte; 32]);
        let device_id = DeviceId::from_public_key(signing_key.public_key_bytes());
        (
            VaultId::from_bytes([1; 16]),
            ItemId::from_bytes([2; 16]),
            signing_key,
            device_id,
            SecretKey::from_bytes([9; 32]),
        )
    }

    #[test]
    fn local_change_is_signed_encrypted_and_atomically_projected() {
        let (vault_id, item_id, signing_key, device_id, dek) = fixture(3);
        let mut db = Db::open_in_memory().unwrap();
        let change = create_local_change(
            &mut db,
            vault_id,
            item_id,
            &signing_key,
            &dek,
            Operation::Upsert,
            b"secret payload",
            1,
            1_000,
        )
        .unwrap();

        assert_eq!(change.origin_device_id, device_id);
        assert_eq!(change.origin_seq, 1);
        assert!(
            crate::crypto::keys::verify_signature(
                signing_key.public_key_bytes(),
                &change.signed_bytes().unwrap(),
                &change.signature,
            )
            .is_ok()
        );
        let decrypted =
            cipher::decrypt(&dek, &change.aad_context(), &change.encrypted_payload()).unwrap();
        assert_eq!(decrypted.as_bytes(), b"secret payload");

        let projection: (Vec<u8>, i64) = db
            .connection()
            .query_row(
                "SELECT winning_change_id, deleted FROM items WHERE item_id = ?1",
                [item_id.as_ref()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(projection.0, change.change_id.as_ref());
        assert_eq!(projection.1, 0);
        assert_eq!(
            db.connection()
                .query_row(
                    "SELECT highest_contiguous_origin_seq FROM sync_cursors WHERE origin_device_id = ?1",
                    [device_id.as_ref()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        assert_eq!(db.user_version().unwrap(), 1);
    }

    #[test]
    fn received_changes_handle_gaps_duplicates_and_authentication() {
        let (vault_id, item_id, source_key, source_device, dek) = fixture(4);
        let mut source = Db::open_in_memory().unwrap();
        let first = create_local_change(
            &mut source,
            vault_id,
            item_id,
            &source_key,
            &dek,
            Operation::Upsert,
            b"first",
            1,
            1_000,
        )
        .unwrap();
        let second = create_local_change(
            &mut source,
            vault_id,
            item_id,
            &source_key,
            &dek,
            Operation::Upsert,
            b"second",
            1,
            1_001,
        )
        .unwrap();
        assert_eq!(second.parent_change_ids, vec![first.change_id]);

        let mut target = Db::open_in_memory().unwrap();
        assert_eq!(
            apply_received_change(&mut target, &second, source_key.public_key_bytes()).unwrap(),
            super::ApplyResult::Inserted
        );
        assert_eq!(
            target
                .connection()
                .query_row(
                    "SELECT highest_contiguous_origin_seq FROM sync_cursors WHERE origin_device_id = ?1",
                    [source_device.as_ref()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        assert_eq!(
            apply_received_change(&mut target, &first, source_key.public_key_bytes()).unwrap(),
            super::ApplyResult::Inserted
        );
        assert_eq!(
            target
                .connection()
                .query_row(
                    "SELECT highest_contiguous_origin_seq FROM sync_cursors WHERE origin_device_id = ?1",
                    [source_device.as_ref()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            2
        );
        assert_eq!(
            apply_received_change(&mut target, &first, source_key.public_key_bytes()).unwrap(),
            super::ApplyResult::Duplicate
        );

        let mut tampered = second.clone();
        tampered.ciphertext[0] ^= 1;
        assert!(matches!(
            apply_received_change(&mut target, &tampered, source_key.public_key_bytes()),
            Err(JournalError::InvalidSignature)
        ));

        let mut conflicting = first.clone();
        conflicting.change_id = super::ChangeId::from_bytes([8; 16]);
        conflicting.signature = source_key
            .sign(&conflicting.signed_bytes().unwrap())
            .to_vec();
        assert!(matches!(
            apply_received_change(&mut target, &conflicting, source_key.public_key_bytes()),
            Err(JournalError::ConflictingDuplicate)
        ));
    }

    #[test]
    fn local_change_failure_rolls_back_journal_clock_and_projection() {
        let (vault_id, item_id, signing_key, _, dek) = fixture(5);
        let mut db = Db::open_in_memory().unwrap();
        db.connection()
            .execute_batch(
                "CREATE TRIGGER reject_change BEFORE INSERT ON changes BEGIN SELECT RAISE(ABORT, 'test failure'); END;",
            )
            .unwrap();

        assert!(
            create_local_change(
                &mut db,
                vault_id,
                item_id,
                &signing_key,
                &dek,
                Operation::Upsert,
                b"payload",
                1,
                1_000,
            )
            .is_err()
        );

        for table in ["changes", "items", "clock_state"] {
            let count: i64 = db
                .connection()
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(count, 0, "table {table} was partially mutated");
        }
    }

    #[test]
    fn tombstones_store_empty_encrypted_payload_and_increment_sequence() {
        let (vault_id, item_id, signing_key, _, dek) = fixture(6);
        let mut db = Db::open_in_memory().unwrap();
        let change = create_local_change(
            &mut db,
            vault_id,
            item_id,
            &signing_key,
            &dek,
            Operation::Tombstone,
            b"ignored",
            1,
            1_000,
        )
        .unwrap();

        assert_eq!(change.hlc, Hlc::new(1_000, 0));
        assert!(
            cipher::decrypt(&dek, &change.aad_context(), &change.encrypted_payload())
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            db.connection()
                .query_row(
                    "SELECT deleted FROM items WHERE item_id = ?1",
                    [item_id.as_ref()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
    }
}

#[cfg(test)]
mod task5_tests {
    use super::{
        ApplyResult, DeviceId, Ed25519Keypair, ItemId, Operation, SecretKey, VaultId,
        apply_received_change, create_local_change, crypto::cipher, resolve_conflicts, storage::Db,
        unresolved_heads,
    };

    fn key(byte: u8) -> (Ed25519Keypair, DeviceId) {
        let signing = Ed25519Keypair::from_private_bytes([byte; 32]);
        let device = DeviceId::from_public_key(signing.public_key_bytes());
        (signing, device)
    }

    fn base_change() -> (VaultId, ItemId, Ed25519Keypair, SecretKey, super::Change) {
        let vault_id = VaultId::from_bytes([7; 16]);
        let item_id = ItemId::from_bytes([8; 16]);
        let (signing, _) = key(21);
        let dek = SecretKey::from_bytes([31; 32]);
        let mut db = Db::open_in_memory().unwrap();
        let base = create_local_change(
            &mut db,
            vault_id,
            item_id,
            &signing,
            &dek,
            Operation::Upsert,
            b"base",
            1,
            1_000,
        )
        .unwrap();
        (vault_id, item_id, signing, dek, base)
    }

    fn receive_base(
        vault_id: VaultId,
        item_id: ItemId,
        base: &super::Change,
        base_key: &Ed25519Keypair,
        dek: &SecretKey,
    ) -> Db {
        let mut db = Db::open_in_memory().unwrap();
        assert_eq!(
            apply_received_change(&mut db, base, base_key.public_key_bytes()).unwrap(),
            ApplyResult::Inserted
        );
        let _: (VaultId, ItemId, &SecretKey) = (vault_id, item_id, dek);
        db
    }

    #[test]
    fn concurrent_edits_converge_to_the_same_winner_and_conflict() {
        let (vault_id, item_id, base_key, dek, base) = base_change();
        let (left_key, _) = key(22);
        let (right_key, _) = key(23);
        let mut left_branch = receive_base(vault_id, item_id, &base, &base_key, &dek);
        let mut right_branch = receive_base(vault_id, item_id, &base, &base_key, &dek);
        let left = create_local_change(
            &mut left_branch,
            vault_id,
            item_id,
            &left_key,
            &dek,
            Operation::Upsert,
            b"left",
            1,
            2_000,
        )
        .unwrap();
        let right = create_local_change(
            &mut right_branch,
            vault_id,
            item_id,
            &right_key,
            &dek,
            Operation::Upsert,
            b"right",
            1,
            2_000,
        )
        .unwrap();
        assert_eq!(left.parent_change_ids, vec![base.change_id]);
        assert_eq!(right.parent_change_ids, vec![base.change_id]);

        let mut first_order = receive_base(vault_id, item_id, &base, &base_key, &dek);
        apply_received_change(&mut first_order, &left, left_key.public_key_bytes()).unwrap();
        apply_received_change(&mut first_order, &right, right_key.public_key_bytes()).unwrap();
        let mut second_order = receive_base(vault_id, item_id, &base, &base_key, &dek);
        apply_received_change(&mut second_order, &right, right_key.public_key_bytes()).unwrap();
        apply_received_change(&mut second_order, &left, left_key.public_key_bytes()).unwrap();

        let winning_first: Vec<u8> = first_order
            .connection()
            .query_row(
                "SELECT winning_change_id FROM items WHERE item_id = ?1",
                [item_id.as_ref()],
                |row| row.get(0),
            )
            .unwrap();
        let winning_second: Vec<u8> = second_order
            .connection()
            .query_row(
                "SELECT winning_change_id FROM items WHERE item_id = ?1",
                [item_id.as_ref()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(winning_first, winning_second);
        assert_eq!(winning_first, right.change_id.as_ref());
        for db in [&first_order, &second_order] {
            assert_eq!(
                db.connection()
                    .query_row(
                        "SELECT count(*) FROM conflicts WHERE item_id = ?1",
                        [item_id.as_ref()],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap(),
                1
            );
        }
    }

    #[test]
    fn edit_delete_branches_use_the_same_deterministic_merge_rule() {
        let (vault_id, item_id, base_key, dek, base) = base_change();
        let (edit_key, _) = key(24);
        let (delete_key, _) = key(25);
        let mut edit_branch = receive_base(vault_id, item_id, &base, &base_key, &dek);
        let mut delete_branch = receive_base(vault_id, item_id, &base, &base_key, &dek);
        let edit = create_local_change(
            &mut edit_branch,
            vault_id,
            item_id,
            &edit_key,
            &dek,
            Operation::Upsert,
            b"edited",
            1,
            2_000,
        )
        .unwrap();
        let deleted = create_local_change(
            &mut delete_branch,
            vault_id,
            item_id,
            &delete_key,
            &dek,
            Operation::Tombstone,
            b"ignored",
            1,
            2_000,
        )
        .unwrap();

        let mut merged = receive_base(vault_id, item_id, &base, &base_key, &dek);
        apply_received_change(&mut merged, &edit, edit_key.public_key_bytes()).unwrap();
        apply_received_change(&mut merged, &deleted, delete_key.public_key_bytes()).unwrap();
        let projection: (Vec<u8>, i64) = merged
            .connection()
            .query_row(
                "SELECT winning_change_id, deleted FROM items WHERE item_id = ?1",
                [item_id.as_ref()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let expected = [&edit, &deleted]
            .into_iter()
            .max_by_key(|change| {
                super::HlcOrderKey::new(change.hlc, change.origin_device_id, change.origin_seq)
            })
            .unwrap();
        assert_eq!(projection.0, expected.change_id.as_ref());
        assert_eq!(projection.1, i64::from(expected.is_tombstone()));
        assert_eq!(
            merged
                .connection()
                .query_row(
                    "SELECT count(*) FROM conflicts WHERE item_id = ?1",
                    [item_id.as_ref()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
    }

    #[test]
    fn descendant_changes_clear_their_ancestor_from_conflicts() {
        let (vault_id, item_id, base_key, dek, base) = base_change();
        let (branch_key, _) = key(26);
        let mut db = receive_base(vault_id, item_id, &base, &base_key, &dek);
        let branch = create_local_change(
            &mut db,
            vault_id,
            item_id,
            &branch_key,
            &dek,
            Operation::Upsert,
            b"descendant",
            1,
            2_000,
        )
        .unwrap();
        assert_eq!(
            db.connection()
                .query_row(
                    "SELECT count(*) FROM conflicts WHERE item_id = ?1",
                    [item_id.as_ref()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
        assert_eq!(branch.parent_change_ids, vec![base.change_id]);
    }

    #[test]
    fn manual_resolution_creates_a_multi_parent_revision_with_verbatim_payload() {
        let (vault_id, item_id, base_key, dek, base) = base_change();
        let (left_key, _) = key(28);
        let (right_key, _) = key(29);
        let (resolver_key, _) = key(30);
        let mut left_branch = receive_base(vault_id, item_id, &base, &base_key, &dek);
        let mut right_branch = receive_base(vault_id, item_id, &base, &base_key, &dek);
        let left = create_local_change(
            &mut left_branch,
            vault_id,
            item_id,
            &left_key,
            &dek,
            Operation::Upsert,
            b"selected payload",
            1,
            2_000,
        )
        .unwrap();
        let right = create_local_change(
            &mut right_branch,
            vault_id,
            item_id,
            &right_key,
            &dek,
            Operation::Upsert,
            b"discarded payload",
            1,
            2_000,
        )
        .unwrap();

        let mut merged = receive_base(vault_id, item_id, &base, &base_key, &dek);
        apply_received_change(&mut merged, &left, left_key.public_key_bytes()).unwrap();
        apply_received_change(&mut merged, &right, right_key.public_key_bytes()).unwrap();
        let mut expected_parents = unresolved_heads(&merged, item_id)
            .unwrap()
            .into_iter()
            .map(|change| change.change_id)
            .collect::<Vec<_>>();
        expected_parents.sort_unstable();
        let resolution = resolve_conflicts(
            &mut merged,
            vault_id,
            item_id,
            left.change_id,
            &resolver_key,
            &dek,
            3_000,
        )
        .unwrap();

        assert_eq!(resolution.parent_change_ids, expected_parents);
        let selected_payload =
            cipher::decrypt(&dek, &left.aad_context(), &left.encrypted_payload()).unwrap();
        let resolved_payload = cipher::decrypt(
            &dek,
            &resolution.aad_context(),
            &resolution.encrypted_payload(),
        )
        .unwrap();
        assert_eq!(resolved_payload.as_bytes(), selected_payload.as_bytes());
        assert_eq!(
            merged
                .connection()
                .query_row(
                    "SELECT count(*) FROM conflicts WHERE item_id = ?1",
                    [item_id.as_ref()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
    }
}

#[cfg(test)]
mod task6_tests {
    use super::{
        BackupError, DeviceIdentity, Ed25519Keypair, Hlc, ItemId, Operation, SecretKey, VaultId,
        X25519Keypair, create_genesis, create_local_change, export_backup, restore_backup,
        storage::Db,
    };
    fn source(with_membership: bool) -> (Db, VaultId, ItemId, SecretKey) {
        let vault_id = VaultId::from_bytes([41; 16]);
        let item_id = ItemId::from_bytes([42; 16]);
        let key = SecretKey::from_bytes([43; 32]);
        let signing = Ed25519Keypair::from_private_bytes([44; 32]);
        let mut db = Db::open_in_memory().unwrap();
        create_local_change(
            &mut db,
            vault_id,
            item_id,
            &signing,
            &key,
            Operation::Upsert,
            b"backup secret",
            1,
            1_000,
        )
        .unwrap();
        if with_membership {
            let creator = DeviceIdentity::new_signed(
                "backup",
                1,
                &signing,
                X25519Keypair::from_private_bytes([45; 32]).public_key(),
            )
            .unwrap();
            let record = create_genesis(vault_id, creator, Hlc::new(1, 0), &signing).unwrap();
            let bytes = record.to_canonical_bytes().unwrap();
            let hash = record.record_hash().unwrap();
            db.connection()
                .execute(
                    "INSERT INTO memberships (record_hash, vault_id, record_type, record) VALUES (?1, ?2, 'genesis', ?3)",
                    (hash.as_ref(), vault_id.as_ref(), bytes),
                )
                .unwrap();
        }
        (db, vault_id, item_id, key)
    }

    #[test]
    fn encrypted_backup_round_trips_into_a_fresh_profile() {
        let (source, vault_id, item_id, key) = source(true);
        let archive = export_backup(&source, &key).unwrap();
        assert!(
            !archive
                .windows(b"backup secret".len())
                .any(|window| window == b"backup secret")
        );

        let mut destination = Db::open_in_memory().unwrap();
        let result = restore_backup(&archive, &mut destination, &key).unwrap();
        assert_eq!(result.vault_id, vault_id);
        assert!(!result.fresh_vault);
        assert_eq!(result.imported_changes, 1);
        assert_eq!(
            destination
                .connection()
                .query_row("SELECT count(*) FROM changes", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            destination
                .connection()
                .query_row(
                    "SELECT count(*) FROM items WHERE item_id = ?1",
                    [item_id.as_ref()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        assert_eq!(
            destination
                .connection()
                .query_row("SELECT count(*) FROM local_device", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn no_membership_restore_creates_a_new_vault_genesis_and_state() {
        let (source, vault_id, item_id, key) = source(false);
        let archive = export_backup(&source, &key).unwrap();
        let mut destination = Db::open_in_memory().unwrap();
        let result = restore_backup(&archive, &mut destination, &key).unwrap();

        assert!(result.fresh_vault);
        assert_ne!(result.vault_id, vault_id);
        assert_eq!(result.imported_changes, 1);
        let restored_vault: Vec<u8> = destination
            .connection()
            .query_row("SELECT vault_id FROM changes LIMIT 1", [], |row| row.get(0))
            .unwrap();
        assert_eq!(restored_vault, result.vault_id.as_ref());
        assert_eq!(
            destination
                .connection()
                .query_row("SELECT record_type FROM memberships LIMIT 1", [], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap(),
            "genesis"
        );
        assert_eq!(
            destination
                .connection()
                .query_row(
                    "SELECT count(*) FROM items WHERE item_id = ?1",
                    [item_id.as_ref()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
    }

    #[test]
    fn malformed_or_tampered_archive_never_replaces_existing_state() {
        let (source, _, _, key) = source(true);
        let archive = export_backup(&source, &key).unwrap();
        let mut destination = Db::open_in_memory().unwrap();
        let (sentinel_vault, sentinel_item, sentinel_key) = {
            let vault = VaultId::from_bytes([51; 16]);
            let item = ItemId::from_bytes([52; 16]);
            let signing = Ed25519Keypair::from_private_bytes([53; 32]);
            let key = SecretKey::from_bytes([54; 32]);
            create_local_change(
                &mut destination,
                vault,
                item,
                &signing,
                &key,
                Operation::Upsert,
                b"sentinel",
                1,
                1_000,
            )
            .unwrap();
            (vault, item, key)
        };
        let mut tampered = archive.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(matches!(
            restore_backup(&tampered, &mut destination, &key),
            Err(BackupError::InvalidArchive(_))
        ));
        let row: (Vec<u8>, Vec<u8>) = destination
            .connection()
            .query_row("SELECT vault_id, item_id FROM changes LIMIT 1", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(row.0, sentinel_vault.as_ref());
        assert_eq!(row.1, sentinel_item.as_ref());
        let _ = sentinel_key;
    }
}
