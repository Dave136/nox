use super::Settings;
use std::{
    fs::{self, File},
    io::{self, Write},
    path::Path,
};

pub(crate) fn load_settings(data_dir: &Path) -> Settings {
    load_settings_file(&Settings::path(data_dir))
        .or_else(|| load_settings_file(&Settings::sidecar_path(&data_dir.join("vault.db"))))
        .unwrap_or_default()
}

fn load_settings_file(path: &Path) -> Option<Settings> {
    match fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
    {
        Some(mut settings) if settings.version == super::model::SETTINGS_VERSION => {
            settings.normalize();
            Some(settings)
        }
        _ => None,
    }
}

pub(crate) fn save_settings(data_dir: &Path, settings: &Settings) -> io::Result<()> {
    let path = Settings::path(data_dir);
    let temp = path.with_extension("tmp");
    let bytes = serde_json::to_vec_pretty(settings).map_err(io::Error::other)?;
    let mut file = File::create(&temp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn app_global_round_trip_preserves_settings() {
        let path = std::env::temp_dir().join(format!("nox-settings-{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        let settings = Settings {
            auto_lock_seconds: 120,
            ..Settings::default()
        };
        save_settings(&path, &settings).unwrap();
        assert_eq!(load_settings(&path), settings);
        assert!(path.join("settings.json").exists());
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn legacy_sidecar_settings_migrate_to_the_app_global_file() {
        let dir =
            std::env::temp_dir().join(format!("nox-settings-migration-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let legacy_vault = dir.join("vault.db");
        std::fs::write(&legacy_vault, []).unwrap();
        std::fs::write(
            legacy_vault.with_extension("settings.json"),
            br#"{"version":1,"auto_lock_seconds":120,"clipboard_seconds":15,"sync_system_theme":false,"bright_colors":true,"transparency_percent":100,"background_blur":true,"dim_inactive_panes":true,"language":"English","autofill_enabled":false,"notifications_enabled":false}"#,
        )
        .unwrap();

        assert_eq!(load_settings(&dir).auto_lock_seconds, 120);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn settings_missing_lock_on_suspend_field_defaults_to_true_and_preserves_other_fields() {
        let dir = std::env::temp_dir().join(format!(
            "nox-settings-missing-lock-on-suspend-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            Settings::path(&dir),
            br#"{"version":1,"auto_lock_seconds":120,"clipboard_seconds":15,"sync_system_theme":false,"bright_colors":true,"transparency_percent":100,"background_blur":true,"dim_inactive_panes":true,"language":"English","autofill_enabled":false,"notifications_enabled":false}"#,
        )
        .unwrap();

        let loaded = load_settings(&dir);
        assert_eq!(loaded.auto_lock_seconds, 120);
        assert!(loaded.lock_on_suspend);
        let _ = std::fs::remove_dir_all(dir);
    }
}
