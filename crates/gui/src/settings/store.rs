use super::Settings;
use std::{
    fs::{self, File},
    io::{self, Write},
    path::Path,
};

pub(crate) fn load_settings(vault_path: &Path) -> Settings {
    let path = Settings::sidecar_path(vault_path);
    match fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
    {
        Some(mut settings) if settings.version == super::model::SETTINGS_VERSION => {
            settings.normalize();
            settings
        }
        _ => Settings::default(),
    }
}

pub(crate) fn save_settings(vault_path: &Path, settings: &Settings) -> io::Result<()> {
    let path = Settings::sidecar_path(vault_path);
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
    fn sidecar_round_trip_preserves_settings() {
        let path = std::env::temp_dir().join(format!("nox-settings-{}", std::process::id()));
        let settings = Settings {
            auto_lock_seconds: 120,
            ..Settings::default()
        };
        save_settings(&path, &settings).unwrap();
        assert_eq!(load_settings(&path), settings);
        let _ = std::fs::remove_file(Settings::sidecar_path(&path));
    }
}
