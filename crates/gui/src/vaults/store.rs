use super::model::{REGISTRY_VERSION, VaultEntry, VaultRegistry};
use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
};

pub(crate) struct RegistryLoad {
    pub(crate) registry: VaultRegistry,
    pub(crate) unreadable: bool,
}

pub(crate) fn registry_path(data_dir: &Path) -> PathBuf {
    data_dir.join("vaults.json")
}

pub(crate) fn load_registry(data_dir: &Path) -> RegistryLoad {
    match fs::read(registry_path(data_dir)) {
        Ok(bytes) => match serde_json::from_slice::<VaultRegistry>(&bytes) {
            Ok(registry) if registry.version == REGISTRY_VERSION => RegistryLoad {
                registry,
                unreadable: false,
            },
            _ => RegistryLoad {
                registry: VaultRegistry::default(),
                unreadable: true,
            },
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => RegistryLoad {
            registry: VaultRegistry::default(),
            unreadable: false,
        },
        Err(_) => RegistryLoad {
            registry: VaultRegistry::default(),
            unreadable: true,
        },
    }
}

pub(crate) fn save_registry(data_dir: &Path, registry: &VaultRegistry) -> io::Result<()> {
    let path = registry_path(data_dir);
    let temp = path.with_extension("tmp");
    let bytes = serde_json::to_vec_pretty(registry).map_err(io::Error::other)?;
    let mut file = create_owner_only(&temp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temp, path)
}

pub(crate) fn adopt_legacy_vault(data_dir: &Path, registry: &mut VaultRegistry) -> bool {
    let path = data_dir.join("vault.db");
    if !path.is_file() || registry.vaults.iter().any(|entry| entry.path == path) {
        return false;
    }

    registry
        .vaults
        .push(VaultEntry::new("Personal vault", path));
    true
}

#[cfg(unix)]
fn create_owner_only(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

#[cfg(not(unix))]
fn create_owner_only(path: &Path) -> io::Result<File> {
    File::create(path)
}

#[cfg(test)]
mod tests {
    use crate::vaults::{
        VaultEntry, VaultRegistry, adopt_legacy_vault, load_registry, registry_path, save_registry,
        slug_for,
    };
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nox-vault-registry-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn registry_round_trip_preserves_entries() {
        let dir = temp_dir("round-trip");
        let registry = VaultRegistry {
            vaults: vec![VaultEntry::new("Personal vault", dir.join("vault.db"))],
            ..VaultRegistry::default()
        };
        save_registry(&dir, &registry).unwrap();
        assert_eq!(load_registry(&dir).registry, registry);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unreadable_registry_is_preserved_and_reported() {
        let dir = temp_dir("invalid");
        fs::write(registry_path(&dir), b"not json").unwrap();
        let load = load_registry(&dir);
        assert!(load.unreadable);
        assert!(load.registry.vaults.is_empty());
        assert_eq!(fs::read(registry_path(&dir)).unwrap(), b"not json");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn slugs_are_derived_and_deduplicated() {
        let existing = vec![VaultEntry::new("Personal vault", "/x/vault.db")];
        assert_eq!(slug_for("Personal vault", &[]), "personal-vault");
        assert_eq!(slug_for("Personal vault", &existing), "personal-vault-2");
    }

    #[test]
    fn a_legacy_vault_is_adopted_once_and_left_in_place() {
        let dir = temp_dir("adopt");
        let legacy = dir.join("vault.db");
        fs::write(&legacy, []).unwrap();
        let mut registry = VaultRegistry::default();

        assert!(adopt_legacy_vault(&dir, &mut registry));
        assert_eq!(registry.vaults.len(), 1);
        assert_eq!(registry.vaults[0].path, legacy);
        assert!(legacy.exists(), "the legacy vault must never be moved");

        assert!(!adopt_legacy_vault(&dir, &mut registry));
        assert_eq!(registry.vaults.len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }
}
