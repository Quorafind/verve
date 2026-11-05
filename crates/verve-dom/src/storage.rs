//! Where `localStorage` lives between runs.

use std::path::PathBuf;

/// Where `localStorage` lives between runs.
pub trait StorageBackend {
    fn load(&self) -> std::collections::BTreeMap<String, String>;
    fn save(&self, items: &std::collections::BTreeMap<String, String>);
}

/// `localStorage` kept as one JSON file.
pub struct FileStorage(pub PathBuf);

impl StorageBackend for FileStorage {
    fn load(&self) -> std::collections::BTreeMap<String, String> {
        std::fs::read_to_string(&self.0)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn save(&self, items: &std::collections::BTreeMap<String, String>) {
        if let Some(parent) = self.0.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string(items) {
            let _ = std::fs::write(&self.0, text);
        }
    }
}
