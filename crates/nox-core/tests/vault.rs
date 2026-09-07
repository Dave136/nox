use nox_core::{
    ITEM_SCHEMA_VERSION, IconChoice, ItemId, ItemPayload, ItemType, NoteColor, Vault, VaultError,
};
use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

fn temp_path(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir()
        .join(format!("locker-vault-{label}-{nonce}"))
        .join("vault.db")
}

fn remove(path: &PathBuf) {
    let _ = fs::remove_file(path);
    if let Some(parent) = path.parent() {
        let _ = fs::remove_dir(parent);
    }
}

fn payload(title: &str, password: &str) -> ItemPayload {
    ItemPayload {
        schema_version: ITEM_SCHEMA_VERSION,
        item_type: ItemType::Login,
        title: title.into(),
        username: "alice".into(),
        password: password.into(),
        uris: vec!["https://example.test".into()],
        notes: "notes".into(),
        created_at: 10,
        updated_at: 11,
        icon: IconChoice::Default,
        note_color: NoteColor::Blue,
    }
}

#[test]
fn create_unlock_and_lock_preserve_identity() {
    let path = temp_path("roundtrip");
    let created = Vault::create(b"password", &path).unwrap();
    let vault_id = created.vault_id();
    let device_id = created.device_id();
    created.lock();

    let unlocked = Vault::unlock(b"password", &path).unwrap();
    assert_eq!(unlocked.vault_id(), vault_id);
    assert_eq!(unlocked.device_id(), device_id);
    unlocked.lock();
    remove(&path);
}

#[test]
fn wrong_password_and_wrapped_dek_tampering_are_indistinguishable() {
    let path = temp_path("auth");
    let vault = Vault::create(b"password", &path).unwrap();
    vault.lock();

    let wrong = Vault::unlock(b"wrong", &path).unwrap_err();
    let db = nox_core::storage::Db::open(&path).unwrap();
    db.connection()
        .execute(
            "UPDATE vault_meta SET wrapped_dek = zeroblob(length(wrapped_dek))",
            [],
        )
        .unwrap();
    let tampered = Vault::unlock(b"password", &path).unwrap_err();
    assert!(matches!(wrong, VaultError::IncorrectPasswordOrCorruptVault));
    assert!(matches!(
        tampered,
        VaultError::IncorrectPasswordOrCorruptVault
    ));
    remove(&path);
}

#[test]
fn unlock_missing_path_does_not_create_a_database() {
    let path = temp_path("missing");
    assert!(!path.exists());
    assert!(matches!(
        Vault::unlock(b"password", &path),
        Err(VaultError::VaultNotFound)
    ));
    assert!(!path.exists());
    remove(&path);
}

#[test]
fn unlock_rejects_unsafe_argon2_header_without_deriving() {
    let path = temp_path("header");
    let vault = Vault::create(b"password", &path).unwrap();
    vault.lock();
    let db = nox_core::storage::Db::open(&path).unwrap();
    db.connection()
        .execute("UPDATE vault_meta SET argon2_memory_kib = ?1", [u32::MAX])
        .unwrap();
    assert!(matches!(
        Vault::unlock(b"password", &path),
        Err(VaultError::IncorrectPasswordOrCorruptVault)
    ));
    remove(&path);
}

#[test]
fn create_twice_reports_already_exists_without_replacing_vault() {
    let path = temp_path("duplicate");
    let vault = Vault::create(b"password", &path).unwrap();
    let vault_id = vault.vault_id();
    vault.lock();
    assert!(matches!(
        Vault::create(b"other", &path),
        Err(VaultError::VaultAlreadyExists)
    ));
    assert_eq!(
        Vault::unlock(b"password", &path).unwrap().vault_id(),
        vault_id
    );
    remove(&path);
}

#[test]
fn item_crud_and_durable_restore_use_only_the_vault_facade() {
    let path = temp_path("crud");
    let first = payload("first", "secret");
    let updated = payload("updated", "new-secret");
    let mut vault = Vault::create(b"password", &path).unwrap();
    let item_id = vault.create_item(&first).unwrap();
    assert_eq!(vault.get_item(item_id).unwrap(), Some(first.clone()));
    assert_eq!(vault.list_items().unwrap(), vec![(item_id, first.clone())]);

    vault.update_item(item_id, &updated).unwrap();
    assert_eq!(vault.get_item(item_id).unwrap(), Some(updated.clone()));
    vault.delete_item(item_id).unwrap();
    assert_eq!(vault.get_item(item_id).unwrap(), None);
    assert_eq!(vault.list_deleted_items().unwrap(), vec![item_id]);
    assert_eq!(
        vault.last_known_payload(item_id).unwrap(),
        Some(updated.clone())
    );

    vault.lock();
    let mut reopened = Vault::unlock(b"password", &path).unwrap();
    assert_eq!(reopened.list_deleted_items().unwrap(), vec![item_id]);
    assert_eq!(
        reopened.last_known_payload(item_id).unwrap(),
        Some(updated.clone())
    );
    reopened.update_item(item_id, &first).unwrap();
    assert_eq!(reopened.list_items().unwrap(), vec![(item_id, first)]);
    reopened.lock();
    remove(&path);
}

#[test]
fn update_and_delete_reject_unknown_items_without_writing() {
    let path = temp_path("missing-item");
    let mut vault = Vault::create(b"password", &path).unwrap();
    let missing = ItemId::new();
    assert!(matches!(
        vault.update_item(missing, &payload("missing", "secret")),
        Err(VaultError::ItemNotFound)
    ));
    assert!(matches!(
        vault.delete_item(missing),
        Err(VaultError::ItemNotFound)
    ));
    assert!(vault.list_items().unwrap().is_empty());
    assert!(vault.list_deleted_items().unwrap().is_empty());
    vault.lock();
    remove(&path);
}
