use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

/// Persisted set of group IDs that should be re-hosted on startup.
#[derive(Default, Serialize, Deserialize)]
pub struct AutoHostStore {
    pub ids: HashSet<String>,
}

fn store_path() -> Option<PathBuf> {
    let appdata = std::env::var("APPDATA").ok()?;
    Some(
        PathBuf::from(appdata)
            .join("devtunnel-gui")
            .join("auto-host.json"),
    )
}

/// Loads the persisted auto-host set from `%APPDATA%/devtunnel-gui/auto-host.json`.
/// Returns an empty store if the file is missing or cannot be parsed.
pub fn load() -> AutoHostStore {
    let path = match store_path() {
        Some(p) => p,
        None => return AutoHostStore::default(),
    };
    match std::fs::read_to_string(&path) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => AutoHostStore::default(),
    }
}

/// Writes the auto-host set to `%APPDATA%/devtunnel-gui/auto-host.json`,
/// creating the directory if it does not exist. Errors are silently ignored.
pub fn save(store: &AutoHostStore) {
    let Some(path) = store_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(store) {
        let _ = std::fs::write(&path, json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let mut store = AutoHostStore::default();
        store.ids.insert("tunnel-abc".into());
        store.ids.insert("tunnel-xyz".into());

        let json = serde_json::to_string(&store).unwrap();
        let restored: AutoHostStore = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.ids, store.ids);
    }

    #[test]
    fn empty_roundtrip() {
        let store = AutoHostStore::default();
        let json = serde_json::to_string(&store).unwrap();
        let restored: AutoHostStore = serde_json::from_str(&json).unwrap();
        assert!(restored.ids.is_empty());
    }

    #[test]
    fn invalid_json_returns_default() {
        let result: Result<AutoHostStore, _> = serde_json::from_str("not-valid-json");
        let store = result.unwrap_or_default();
        assert!(store.ids.is_empty());
    }
}
