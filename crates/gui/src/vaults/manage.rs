//! Removing a vault: dropping its registry entry, and — only when the user
//! explicitly asks — deleting its files.
//!
//! These are deliberately free functions over [`VaultRegistry`] and [`Path`]
//! with no GPUI types in their signatures, so the code that can destroy a
//! user's only copy of their passwords is covered by plain `#[test]`s rather
//! than by a test that has to spin up a window.

use super::model::{VaultEntry, VaultRegistry};
use crate::settings::Settings;
use std::{fs, path::Path};

/// Files that belong to a vault database and must go with it.
///
/// The `-wal` and `-shm` sidecars are SQLite's; a surviving `-wal` still holds
/// committed pages, so leaving one behind would leave vault data on disk after
/// the user asked for the vault to be deleted.
const SIDECAR_SUFFIXES: [&str; 2] = ["-wal", "-shm"];

/// Drop the entry at `index` and return it, or `None` when the index is out of
/// range. Clears `last_opened` when it pointed at the removed vault — a
/// dangling pointer would make the next launch preselect a vault that is no
/// longer registered.
///
/// Touches no files: see [`delete_vault_files`] for that, which the caller
/// invokes separately and only when the user explicitly opts in.
pub(crate) fn remove_entry(registry: &mut VaultRegistry, index: usize) -> Option<VaultEntry> {
    if index >= registry.vaults.len() {
        return None;
    }
    let removed = registry.vaults.remove(index);
    if registry.last_opened.as_deref() == Some(removed.id.as_str()) {
        registry.last_opened = None;
    }
    Some(removed)
}

/// Delete a vault database along with its SQLite sidecars and its settings
/// file. Missing files are ignored.
///
/// Every `io::Error` is swallowed on purpose. A partially deleted vault is not
/// recoverable either way, and surfacing a failure the user cannot act on adds
/// nothing to a dialog they have already confirmed.
pub(crate) fn delete_vault_files(path: &Path) {
    let _ = fs::remove_file(path);

    for suffix in SIDECAR_SUFFIXES {
        // Appended to the full filename, not swapped for the extension:
        // SQLite names these `vault.db-wal`, not `vault-wal.db`.
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        let _ = fs::remove_file(sidecar);
    }

    // Asks `Settings` for the name rather than rebuilding it here, so a change
    // to the sidecar's naming cannot silently start leaving files behind.
    let _ = fs::remove_file(Settings::sidecar_path(path));
}

#[cfg(test)]
mod tests {
    use crate::vaults::{VaultEntry, VaultRegistry, delete_vault_files, remove_entry};
    use std::{
        fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nox-vault-manage-{name}-{}-{}",
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
    fn removing_an_entry_leaves_every_file_untouched() {
        let dir = temp_dir("remove-entry");
        let path = dir.join("vault.db");
        fs::write(&path, []).unwrap();
        let mut registry = VaultRegistry {
            vaults: vec![
                VaultEntry::new("Personal vault", &path),
                VaultEntry::new("Work vault", dir.join("work.db")),
            ],
            ..VaultRegistry::default()
        };

        let removed = remove_entry(&mut registry, 0).unwrap();

        assert_eq!(registry.vaults.len(), 1);
        assert_eq!(registry.vaults[0].name, "Work vault");
        // Removing from the list is metadata-only: the file must survive.
        assert!(path.exists());
        assert_eq!(removed.path, path);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn removing_out_of_range_changes_nothing() {
        let dir = temp_dir("remove-oob");
        let mut registry = VaultRegistry {
            vaults: vec![VaultEntry::new("Personal vault", dir.join("vault.db"))],
            ..VaultRegistry::default()
        };

        assert!(remove_entry(&mut registry, 7).is_none());
        assert_eq!(registry.vaults.len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn removing_the_last_opened_entry_clears_the_pointer() {
        let dir = temp_dir("remove-last-opened");
        let entry = VaultEntry::new("Personal vault", dir.join("vault.db"));
        let mut registry = VaultRegistry {
            last_opened: Some(entry.id.clone()),
            vaults: vec![entry],
            ..VaultRegistry::default()
        };

        remove_entry(&mut registry, 0).unwrap();

        // A dangling `last_opened` would make the next launch preselect a
        // vault that is no longer registered.
        assert!(registry.last_opened.is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn removing_a_different_entry_keeps_the_last_opened_pointer() {
        let dir = temp_dir("remove-keeps-pointer");
        let kept = VaultEntry::new("Personal vault", dir.join("vault.db"));
        let dropped = VaultEntry::new("Work vault", dir.join("work.db"));
        let mut registry = VaultRegistry {
            last_opened: Some(kept.id.clone()),
            vaults: vec![kept.clone(), dropped],
            ..VaultRegistry::default()
        };

        remove_entry(&mut registry, 1).unwrap();

        assert_eq!(registry.last_opened, Some(kept.id));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn deleting_vault_files_takes_the_sidecars_too() {
        let dir = temp_dir("delete-files");
        let path = dir.join("vault.db");
        for suffix in ["", "-wal", "-shm"] {
            fs::write(format!("{}{suffix}", path.display()), []).unwrap();
        }
        fs::write(path.with_extension("settings.json"), []).unwrap();

        delete_vault_files(&path);

        // A surviving -wal holds committed pages: leaving it behind leaves
        // vault data on disk after the user asked for deletion.
        for suffix in ["", "-wal", "-shm"] {
            let sidecar = format!("{}{suffix}", path.display());
            assert!(!Path::new(&sidecar).exists(), "{sidecar} survived");
        }
        assert!(!path.with_extension("settings.json").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn deleting_a_vault_leaves_its_neighbours_alone() {
        let dir = temp_dir("delete-neighbours");
        let target = dir.join("vault.db");
        let neighbour = dir.join("other.db");
        fs::write(&target, []).unwrap();
        fs::write(&neighbour, []).unwrap();
        // A sidecar belonging to the *neighbour* must not be swept up.
        fs::write(dir.join("other.db-wal"), []).unwrap();

        delete_vault_files(&target);

        assert!(!target.exists());
        assert!(neighbour.exists());
        assert!(dir.join("other.db-wal").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn deleting_an_already_missing_vault_is_a_no_op() {
        let dir = temp_dir("delete-missing");
        // The entry may point at a file that is already gone; this must not
        // panic and must not report failure.
        delete_vault_files(&dir.join("gone.db"));
        fs::remove_dir_all(dir).unwrap();
    }
}
