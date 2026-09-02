#![cfg(unix)]

use nox_core::{
    Ed25519Keypair, ITEM_SCHEMA_VERSION, IconChoice, ItemId, ItemPayload, ItemType, Operation,
    SecretKey, Vault, VaultId, export_to_path, restore_from_path, storage::Db,
};
use std::{
    fs, io,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

static NEXT_TREE: AtomicUsize = AtomicUsize::new(0);

struct TestTree {
    root: PathBuf,
}

impl TestTree {
    fn new(case: &str) -> io::Result<Self> {
        let id = NEXT_TREE.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("locker-task-b-{case}-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir(&root)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        Ok(Self { root })
    }
}

impl Drop for TestTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn permission_bits(path: &Path) -> io::Result<u32> {
    Ok(fs::symlink_metadata(path)?.permissions().mode() & 0o7777)
}

fn assert_regular_owner_mode(path: &Path, root: &Path, expected_mode: u32) {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(metadata.file_type().is_file(), "{path:?} is not regular");
    assert_eq!(metadata.uid(), fs::symlink_metadata(root).unwrap().uid());
    assert_eq!(permission_bits(path).unwrap(), expected_mode);
}

fn assert_directory_owner_mode(path: &Path, root: &Path, expected_mode: u32) {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(metadata.file_type().is_dir(), "{path:?} is not a directory");
    assert_eq!(metadata.uid(), fs::symlink_metadata(root).unwrap().uid());
    assert_eq!(permission_bits(path).unwrap(), expected_mode);
}

fn sqlite_sidecar(database: &Path, suffix: &str) -> PathBuf {
    let mut value = database.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn wait_for_regular(path: &Path) {
    for _ in 0..50 {
        if fs::symlink_metadata(path)
            .map(|metadata| metadata.file_type().is_file())
            .unwrap_or(false)
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("sidecar was not created: {path:?}");
}

fn enter_umask_matrix(case_name: &'static str) -> Option<u32> {
    if let Ok(mode) = std::env::var("NOX_UMASK_CHILD") {
        return Some(u32::from_str_radix(&mode, 8).unwrap());
    }
    let executable = std::env::current_exe().unwrap();
    for mode in ["000", "022", "077", "777"] {
        let status = Command::new("/bin/sh")
            .args([
                "-c",
                "umask \"$1\"; shift; exec \"$@\"",
                "locker-umask",
                mode,
            ])
            .arg(&executable)
            .args(["--exact", case_name, "--nocapture"])
            .env("NOX_UMASK_CHILD", mode)
            .status()
            .unwrap();
        assert!(status.success(), "umask child {mode} failed");
    }
    None
}

#[test]
fn vault_creation_normalizes_owner_only_modes() {
    let Some(_mode) = enter_umask_matrix("vault_creation_normalizes_owner_only_modes") else {
        return;
    };
    let tree = TestTree::new("create").unwrap();
    let database = tree.root.join("nested").join("vault.db");
    let vault = Vault::create(b"master", &database).unwrap();
    assert_directory_owner_mode(database.parent().unwrap(), &tree.root, 0o700);
    assert_regular_owner_mode(&database, &tree.root, 0o600);
    drop(vault);
    Vault::unlock(b"master", &database).unwrap();
}

#[test]
fn vault_open_repairs_permissive_existing_modes() {
    let Some(_mode) = enter_umask_matrix("vault_open_repairs_permissive_existing_modes") else {
        return;
    };
    let tree = TestTree::new("repair").unwrap();
    let database = tree.root.join("vault.db");
    let mut vault = Vault::create(b"master", &database).unwrap();
    vault.create_item(&login_payload()).unwrap();
    drop(vault);
    fs::set_permissions(
        database.parent().unwrap(),
        fs::Permissions::from_mode(0o777),
    )
    .unwrap();
    fs::set_permissions(&database, fs::Permissions::from_mode(0o666)).unwrap();
    let vault = Vault::unlock(b"master", &database).unwrap();
    assert_directory_owner_mode(database.parent().unwrap(), &tree.root, 0o700);
    assert_regular_owner_mode(&database, &tree.root, 0o600);
    assert_eq!(vault.list_items().unwrap().len(), 1);
}

#[test]
fn wal_sidecars_are_owner_only_while_live() {
    let Some(_mode) = enter_umask_matrix("wal_sidecars_are_owner_only_while_live") else {
        return;
    };
    let tree = TestTree::new("wal-live").unwrap();
    let database = tree.root.join("vault.db");
    let first = Db::open(&database).unwrap();
    let second = Db::open(&database).unwrap();
    first
        .connection()
        .execute(
            "CREATE TABLE IF NOT EXISTS permission_probe (value TEXT)",
            [],
        )
        .unwrap();
    first
        .connection()
        .execute("INSERT INTO permission_probe (value) VALUES ('x')", [])
        .unwrap();
    let wal = sqlite_sidecar(&database, "-wal");
    let shm = sqlite_sidecar(&database, "-shm");
    wait_for_regular(&wal);
    wait_for_regular(&shm);
    assert_regular_owner_mode(&wal, &tree.root, 0o600);
    assert_regular_owner_mode(&shm, &tree.root, 0o600);
    drop(second);
    drop(first);
}

#[test]
fn wal_sidecars_existing_modes_are_normalized_before_reopen() {
    let Some(_mode) =
        enter_umask_matrix("wal_sidecars_existing_modes_are_normalized_before_reopen")
    else {
        return;
    };
    let tree = TestTree::new("wal-repair").unwrap();
    let database = tree.root.join("vault.db");
    let first = Db::open(&database).unwrap();
    first
        .connection()
        .execute(
            "CREATE TABLE IF NOT EXISTS permission_probe (value TEXT)",
            [],
        )
        .unwrap();
    first
        .connection()
        .execute("INSERT INTO permission_probe (value) VALUES ('x')", [])
        .unwrap();
    let wal = sqlite_sidecar(&database, "-wal");
    let shm = sqlite_sidecar(&database, "-shm");
    wait_for_regular(&wal);
    wait_for_regular(&shm);
    fs::set_permissions(&wal, fs::Permissions::from_mode(0o666)).unwrap();
    fs::set_permissions(&shm, fs::Permissions::from_mode(0o666)).unwrap();
    let second = Db::open(&database).unwrap();
    assert_regular_owner_mode(&wal, &tree.root, 0o600);
    assert_regular_owner_mode(&shm, &tree.root, 0o600);
    drop(second);
    drop(first);
}

#[test]
fn wal_sidecar_symlinks_are_rejected_without_target_mutation() {
    let Some(_mode) =
        enter_umask_matrix("wal_sidecar_symlinks_are_rejected_without_target_mutation")
    else {
        return;
    };
    let tree = TestTree::new("wal-symlink").unwrap();
    let database = tree.root.join("vault.db");
    let first = Db::open(&database).unwrap();
    drop(first);
    for suffix in ["-wal", "-shm"] {
        let target = tree.root.join(format!("target{suffix}"));
        fs::write(&target, b"sentinel").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
        let sidecar = sqlite_sidecar(&database, suffix);
        let _ = fs::remove_file(&sidecar);
        std::os::unix::fs::symlink(&target, &sidecar).unwrap();
        let error = match Db::open(&database) {
            Ok(_) => panic!("symlinked sidecar was accepted"),
            Err(error) => error,
        };
        assert!(
            matches!(error, nox_core::storage::DbError::Io(ref error) if error.kind() == io::ErrorKind::InvalidInput)
        );
        assert_eq!(fs::read(&target).unwrap(), b"sentinel");
        assert_eq!(permission_bits(&target).unwrap(), 0o644);
        fs::remove_file(sidecar).unwrap();
    }
}

fn login_payload() -> ItemPayload {
    ItemPayload {
        schema_version: ITEM_SCHEMA_VERSION,
        item_type: ItemType::Login,
        title: "permission sentinel title".into(),
        username: "permission sentinel user".into(),
        password: "permission sentinel password".into(),
        uris: vec![],
        notes: String::new(),
        created_at: 1,
        updated_at: 1,
        icon: IconChoice::Default,
    }
}

#[test]
fn backup_export_is_owner_only_and_encrypted() {
    let Some(_mode) = enter_umask_matrix("backup_export_is_owner_only_and_encrypted") else {
        return;
    };
    let tree = TestTree::new("backup-export").unwrap();
    let source = tree.root.join("source.db");
    let archive = tree.root.join("export").join("vault.lockbak");
    let restored = tree.root.join("restored.db");
    fs::create_dir(archive.parent().unwrap()).unwrap();
    fs::set_permissions(archive.parent().unwrap(), fs::Permissions::from_mode(0o755)).unwrap();
    let mut vault = Vault::create(b"master", &source).unwrap();
    vault.create_item(&login_payload()).unwrap();
    vault
        .prepare_backup_export(b"backup-password", &archive)
        .unwrap()
        .run()
        .unwrap();
    let bytes = fs::read(&archive).unwrap();
    assert!(bytes.starts_with(b"NOXBACK2"));
    for sentinel in [
        b"permission sentinel title".as_slice(),
        b"permission sentinel user".as_slice(),
        b"permission sentinel password".as_slice(),
    ] {
        assert!(
            !bytes
                .windows(sentinel.len())
                .any(|window| window == sentinel)
        );
    }
    assert_regular_owner_mode(&archive, &tree.root, 0o600);
    assert!(
        fs::read_dir(archive.parent().unwrap())
            .unwrap()
            .all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .to_string()
                    .contains("tmp-")
            })
    );
    restore_from_path(&archive, &restored, b"backup-password", b"new-master").unwrap();
    let restored = Vault::unlock(b"new-master", &restored).unwrap();
    assert_eq!(restored.list_items().unwrap().len(), 1);
}

#[test]
fn legacy_backup_export_is_owner_only_and_encrypted() {
    let Some(_mode) = enter_umask_matrix("legacy_backup_export_is_owner_only_and_encrypted") else {
        return;
    };
    let tree = TestTree::new("legacy-export").unwrap();
    let archive = tree.root.join("legacy.lockbak");
    let vault_id = VaultId::from_bytes([1; 16]);
    let item_id = ItemId::from_bytes([2; 16]);
    let key = SecretKey::from_bytes([3; 32]);
    let signing = Ed25519Keypair::from_private_bytes([4; 32]);
    let mut db = Db::open_in_memory().unwrap();
    nox_core::create_local_change(
        &mut db,
        vault_id,
        item_id,
        &signing,
        &key,
        Operation::Upsert,
        b"legacy plaintext sentinel",
        1,
        1,
    )
    .unwrap();
    export_to_path(&db, &key, &archive).unwrap();
    assert_regular_owner_mode(&archive, &tree.root, 0o600);
    assert!(
        !fs::read(&archive)
            .unwrap()
            .windows(b"legacy plaintext sentinel".len())
            .any(|window| window == b"legacy plaintext sentinel")
    );
}

#[test]
fn backup_replaces_permissive_destination_atomically() {
    let Some(_mode) = enter_umask_matrix("backup_replaces_permissive_destination_atomically")
    else {
        return;
    };
    let tree = TestTree::new("backup-replace").unwrap();
    let source = tree.root.join("source.db");
    let archive = tree.root.join("export.lockbak");
    let restored = tree.root.join("restored.db");
    let mut vault = Vault::create(b"master", &source).unwrap();
    vault.create_item(&login_payload()).unwrap();
    fs::write(&archive, b"old destination sentinel").unwrap();
    fs::set_permissions(&archive, fs::Permissions::from_mode(0o666)).unwrap();
    vault
        .prepare_backup_export(b"backup-password", &archive)
        .unwrap()
        .run()
        .unwrap();
    assert_regular_owner_mode(&archive, &tree.root, 0o600);
    assert!(
        !fs::read(&archive)
            .unwrap()
            .windows(b"old destination sentinel".len())
            .any(|window| window == b"old destination sentinel")
    );
    restore_from_path(&archive, &restored, b"backup-password", b"new-master").unwrap();
}

#[test]
fn vault_symlinks_are_rejected_without_target_mutation() {
    let Some(_mode) = enter_umask_matrix("vault_symlinks_are_rejected_without_target_mutation")
    else {
        return;
    };
    let tree = TestTree::new("vault-symlink").unwrap();
    let target = tree.root.join("target");
    let target_db = target.join("vault.db");
    Vault::create(b"master", &target_db).unwrap();
    let target_mode = permission_bits(&target).unwrap();
    let link_parent = tree.root.join("parent-link");
    std::os::unix::fs::symlink(&target, &link_parent).unwrap();
    let parent_error = match Db::open(link_parent.join("vault.db")) {
        Ok(_) => panic!("symlinked vault parent was accepted"),
        Err(error) => error,
    };
    assert!(
        matches!(parent_error, nox_core::storage::DbError::Io(ref error) if error.kind() == io::ErrorKind::InvalidInput)
    );
    assert_eq!(permission_bits(&target).unwrap(), target_mode);

    let target_file = tree.root.join("target-file");
    fs::write(&target_file, b"target").unwrap();
    fs::set_permissions(&target_file, fs::Permissions::from_mode(0o644)).unwrap();
    let link_file = tree.root.join("file-link");
    std::os::unix::fs::symlink(&target_file, &link_file).unwrap();
    let leaf_error = match Db::open(&link_file) {
        Ok(_) => panic!("symlinked vault database was accepted"),
        Err(error) => error,
    };
    assert!(
        matches!(leaf_error, nox_core::storage::DbError::Io(ref error) if error.kind() == io::ErrorKind::InvalidInput)
    );
    assert_eq!(fs::read(&target_file).unwrap(), b"target");
}

#[test]
fn backup_destination_symlink_is_replaced_not_followed() {
    let Some(_mode) = enter_umask_matrix("backup_destination_symlink_is_replaced_not_followed")
    else {
        return;
    };
    let tree = TestTree::new("backup-destination-link").unwrap();
    let source = tree.root.join("source.db");
    let destination = tree.root.join("destination.lockbak");
    let target = tree.root.join("sentinel");
    fs::write(&target, b"untouched target").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
    std::os::unix::fs::symlink(&target, &destination).unwrap();
    let vault = Vault::create(b"master", &source).unwrap();
    vault
        .prepare_backup_export(b"backup-password", &destination)
        .unwrap()
        .run()
        .unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"untouched target");
    assert_regular_owner_mode(&destination, &tree.root, 0o600);
}

#[test]
fn backup_parent_symlink_does_not_change_directory_mode() {
    let Some(_mode) = enter_umask_matrix("backup_parent_symlink_does_not_change_directory_mode")
    else {
        return;
    };
    let tree = TestTree::new("backup-parent-link").unwrap();
    let source = tree.root.join("source.db");
    let target = tree.root.join("export-target");
    fs::create_dir(&target).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
    let link = tree.root.join("export-link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let destination = link.join("archive.lockbak");
    let vault = Vault::create(b"master", &source).unwrap();
    vault
        .prepare_backup_export(b"backup-password", &destination)
        .unwrap()
        .run()
        .unwrap();
    assert_eq!(permission_bits(&target).unwrap(), 0o755);
    assert_regular_owner_mode(&target.join("archive.lockbak"), &tree.root, 0o600);
}

/// Not run on macOS: APFS and HFS+ enforce valid UTF-8 in filenames and answer
/// `EILSEQ` for anything else, so a vault directory named with a lone `0x80`
/// cannot be created there at all — `Vault::create` fails before any of the
/// mode assertions below are reachable. The scenario is unreachable rather
/// than unchecked; Linux, where filenames are arbitrary byte sequences, still
/// covers it.
#[cfg(not(target_os = "macos"))]
#[test]
fn non_utf8_vault_name_has_correct_sidecar_checks() {
    // Scoped to this test: it is the only user, and the import would be dead
    // on macOS where the test is compiled out.
    use std::os::unix::ffi::OsStringExt;

    let Some(_mode) = enter_umask_matrix("non_utf8_vault_name_has_correct_sidecar_checks") else {
        return;
    };
    let tree = TestTree::new("non-utf8").unwrap();
    let mut name = std::ffi::OsString::from("vault");
    name.push(std::ffi::OsString::from_vec(vec![0x80]));
    let database = tree.root.join(name).join("vault.db");
    let mut vault = Vault::create(b"master", &database).unwrap();
    vault.create_item(&login_payload()).unwrap();
    let wal = sqlite_sidecar(&database, "-wal");
    let shm = sqlite_sidecar(&database, "-shm");
    wait_for_regular(&wal);
    wait_for_regular(&shm);
    assert_regular_owner_mode(&database, &tree.root, 0o600);
    assert_regular_owner_mode(&wal, &tree.root, 0o600);
    assert_regular_owner_mode(&shm, &tree.root, 0o600);
}
