use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub(crate) const SETTINGS_VERSION: u8 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct Settings {
    pub(crate) version: u8,
    pub(crate) auto_lock_seconds: u64,
    pub(crate) clipboard_seconds: u64,
    pub(crate) sync_system_theme: bool,
    pub(crate) bright_colors: bool,
    pub(crate) transparency_percent: u8,
    pub(crate) background_blur: bool,
    pub(crate) dim_inactive_panes: bool,
    pub(crate) language: String,
    pub(crate) autofill_enabled: bool,
    pub(crate) notifications_enabled: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: SETTINGS_VERSION,
            auto_lock_seconds: 300,
            clipboard_seconds: 30,
            sync_system_theme: false,
            bright_colors: true,
            transparency_percent: 100,
            background_blur: true,
            dim_inactive_panes: true,
            language: "English".into(),
            autofill_enabled: false,
            notifications_enabled: false,
        }
    }
}

impl Settings {
    pub(crate) fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("settings.json")
    }

    pub(crate) fn sidecar_path(vault_path: &Path) -> PathBuf {
        vault_path.with_extension("settings.json")
    }

    pub(crate) fn normalize(&mut self) {
        self.version = SETTINGS_VERSION;
        self.auto_lock_seconds = self.auto_lock_seconds.clamp(30, 3600);
        self.clipboard_seconds = self.clipboard_seconds.clamp(5, 300);
        self.transparency_percent = self.transparency_percent.clamp(60, 100);
        if self.language.trim().is_empty() {
            self.language = "English".into();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalize_clamps_settings() {
        let mut settings = Settings {
            auto_lock_seconds: 1,
            clipboard_seconds: 999,
            transparency_percent: 1,
            ..Settings::default()
        };
        settings.normalize();
        assert_eq!(
            (
                settings.auto_lock_seconds,
                settings.clipboard_seconds,
                settings.transparency_percent
            ),
            (30, 300, 60)
        );
    }
}
