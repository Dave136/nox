use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub(crate) const REGISTRY_VERSION: u8 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct VaultEntry {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) last_opened_ms: Option<u64>,
}

impl VaultEntry {
    pub(crate) fn new(name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        Self {
            id: path.to_string_lossy().into_owned(),
            name: name.into(),
            path,
            last_opened_ms: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct VaultRegistry {
    pub(crate) version: u8,
    pub(crate) vaults: Vec<VaultEntry>,
    pub(crate) last_opened: Option<String>,
}

impl Default for VaultRegistry {
    fn default() -> Self {
        Self {
            version: REGISTRY_VERSION,
            vaults: Vec::new(),
            last_opened: None,
        }
    }
}

pub(crate) fn slug_for(name: &str, existing: &[VaultEntry]) -> String {
    let base = slug(name);
    let mut candidate = base.clone();
    let mut suffix = 2;
    while existing.iter().any(|entry| {
        slug(&entry.name) == candidate
            || entry
                .path
                .parent()
                .and_then(|parent| parent.file_name())
                .is_some_and(|directory| directory == std::ffi::OsStr::new(&candidate))
    }) {
        candidate = format!("{base}-{suffix}");
        suffix += 1;
    }
    candidate
}

fn slug(name: &str) -> String {
    let mut result = String::new();
    let mut separator = false;
    for character in name.chars() {
        if character.is_alphanumeric() {
            if separator && !result.is_empty() {
                result.push('-');
            }
            result.extend(character.to_lowercase());
            separator = false;
        } else {
            separator = true;
        }
    }
    if result.is_empty() {
        "vault".into()
    } else {
        result
    }
}
