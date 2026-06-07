//! Persistent app state (issue #5): the auto-host group set + user settings,
//! stored as JSON under `%APPDATA%\devtunnel-gui\state.json`.
//!
//! Loading is forgiving: a missing or invalid file yields defaults so a corrupt
//! state never blocks startup. Saving is atomic (write to a temp file in the
//! same directory, then rename) so a crash mid-write cannot truncate the state.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// File name of the persisted state inside [`state_dir`].
const STATE_FILE: &str = "state.json";
/// File name of the last-successful-load row cache inside [`state_dir`].
const CACHE_FILE: &str = "cache.json";

/// User-tunable settings persisted across runs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Settings {
    /// Health probe interval in seconds while everything is healthy.
    pub probe_interval_secs: u64,
    /// Whether the app registers itself to start with Windows (HKCU Run key).
    pub auto_start: bool,
    /// Default expiration for new groups and the renewal target (issue #6),
    /// as a free-form CLI string (e.g. `30d`, `12h`). Empty = CLI default.
    pub default_expiration: String,
    /// Dark-mode preference. `None` = follow the Windows app theme (first run);
    /// `Some(_)` = the user picked a mode via the top-bar toggle, persisted.
    pub dark: Option<bool>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            // Conservative default matching the probe engine's steady-state cadence.
            probe_interval_secs: 60,
            auto_start: false,
            // Matches the CLI's maximum tunnel lifetime (30 days).
            default_expiration: "30d".to_string(),
            // First run follows the OS theme until the user chooses explicitly.
            dark: None,
        }
    }
}

/// The persisted application state.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AppState {
    /// Real Tunnel IDs of groups to re-host automatically on startup.
    pub auto_host: Vec<String>,
    pub settings: Settings,
}

impl AppState {
    /// Adds a tunnel id to the auto-host set (dedup, order-preserving).
    pub fn add_auto_host(&mut self, tunnel_id: &str) {
        if !self.contains_auto_host(tunnel_id) {
            self.auto_host.push(tunnel_id.to_string());
        }
    }

    /// Removes a tunnel id from the auto-host set (no-op when absent).
    pub fn remove_auto_host(&mut self, tunnel_id: &str) {
        self.auto_host.retain(|id| id != tunnel_id);
    }

    /// Whether the tunnel id is in the auto-host set.
    pub fn contains_auto_host(&self, tunnel_id: &str) -> bool {
        self.auto_host.iter().any(|id| id == tunnel_id)
    }

    /// Loads the state from `path`. Missing or invalid content yields defaults.
    pub fn load_from(path: &Path) -> AppState {
        match fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                log::warn!("state: invalid {}: {e}; using defaults", path.display());
                AppState::default()
            }),
            Err(_) => AppState::default(),
        }
    }

    /// Saves the state to `path` atomically (temp file + rename). Best-effort:
    /// errors are returned for the caller to log, never to abort the app.
    pub fn save_to(&self, path: &Path) -> anyhow::Result<()> {
        atomic_write(path, &serde_json::to_string_pretty(self)?)
    }

    /// Loads the state from the default location ([`state_path`]).
    pub fn load() -> AppState {
        AppState::load_from(&state_path())
    }

    /// Saves the state to the default location, logging (not propagating) errors.
    pub fn save(&self) {
        let path = state_path();
        if let Err(e) = self.save_to(&path) {
            log::warn!("state: failed to save {}: {e}", path.display());
        }
    }
}

/// The per-user data directory: `%APPDATA%\devtunnel-gui\` (falls back to the
/// current directory if `APPDATA` is unset, which only happens in odd shells).
pub fn state_dir() -> PathBuf {
    match std::env::var_os("APPDATA") {
        Some(appdata) => PathBuf::from(appdata).join("devtunnel-gui"),
        None => PathBuf::from("."),
    }
}

/// Full path of the persisted state file.
pub fn state_path() -> PathBuf {
    state_dir().join(STATE_FILE)
}

/// Full path of the row cache file.
pub fn cache_path() -> PathBuf {
    state_dir().join(CACHE_FILE)
}

/// Writes `content` to `path` atomically: temp file in the same directory,
/// then rename. A crash mid-write can never truncate the destination.
fn atomic_write(path: &Path, content: &str) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, content)?;
    // `rename` replaces the destination atomically on the same volume.
    // On Windows it fails if the destination exists, so remove it first.
    let _ = fs::remove_file(path);
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Loads the cached rows from the last successful load. Missing or invalid
/// content yields an empty list (the async refresh reconciles shortly after).
pub fn load_row_cache() -> Vec<crate::devtunnel::Row> {
    load_row_cache_from(&cache_path())
}

fn load_row_cache_from(path: &Path) -> Vec<crate::devtunnel::Row> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

/// Persists the rows of a successful load so the next startup can paint the
/// list immediately. Best-effort: failures are logged, never propagated.
pub fn save_row_cache(rows: &[crate::devtunnel::Row]) {
    save_row_cache_to(&cache_path(), rows);
}

fn save_row_cache_to(path: &Path, rows: &[crate::devtunnel::Row]) {
    let result = serde_json::to_string(rows)
        .map_err(anyhow::Error::from)
        .and_then(|json| atomic_write(path, &json));
    if let Err(e) = result {
        log::warn!("state: failed to save row cache {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique temp file path per test (no collision across parallel tests).
    fn temp_path(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("devtunnel-gui-test-{}-{name}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir.join(STATE_FILE)
    }

    #[test]
    fn defaults_when_file_missing() {
        let path = temp_path("missing").join("nope.json");
        let st = AppState::load_from(&path);
        assert_eq!(st, AppState::default());
        assert_eq!(st.settings.probe_interval_secs, 60);
        assert!(!st.settings.auto_start);
        assert!(st.auto_host.is_empty());
    }

    #[test]
    fn defaults_when_file_invalid() {
        let path = temp_path("invalid");
        fs::write(&path, "not json at all {{{").unwrap();
        let st = AppState::load_from(&path);
        assert_eq!(st, AppState::default());
    }

    #[test]
    fn round_trip() {
        let path = temp_path("roundtrip");
        let mut st = AppState::default();
        st.add_auto_host("frontend.brs");
        st.settings.auto_start = true;
        st.settings.probe_interval_secs = 30;
        st.settings.default_expiration = "12h".to_string();
        st.save_to(&path).unwrap();

        let loaded = AppState::load_from(&path);
        assert_eq!(loaded, st);
        assert_eq!(loaded.settings.default_expiration, "12h");
    }

    #[test]
    fn save_overwrites_existing() {
        let path = temp_path("overwrite");
        let mut st = AppState::default();
        st.add_auto_host("a.brs");
        st.save_to(&path).unwrap();
        st.remove_auto_host("a.brs");
        st.add_auto_host("b.brs");
        st.save_to(&path).unwrap();

        let loaded = AppState::load_from(&path);
        assert_eq!(loaded.auto_host, vec!["b.brs".to_string()]);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        // Forward-compat: a partial file (older version) still loads.
        let path = temp_path("partial");
        fs::write(&path, r#"{ "auto_host": ["x.brs"] }"#).unwrap();
        let st = AppState::load_from(&path);
        assert!(st.contains_auto_host("x.brs"));
        assert_eq!(st.settings, Settings::default());
        // An older state file without the field falls back to the 30d default.
        assert_eq!(st.settings.default_expiration, "30d");
    }

    #[test]
    fn row_cache_round_trip_and_default_on_missing() {
        let dir = std::env::temp_dir().join(format!(
            "devtunnel-gui-test-{}-rowcache",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(CACHE_FILE);

        // Missing file -> empty list.
        let _ = fs::remove_file(&path);
        assert!(load_row_cache_from(&path).is_empty());

        let rows = vec![crate::devtunnel::Row {
            group: "frontend".into(),
            tunnel_id: "frontend.brs".into(),
            port: 3000,
            protocol: "http".into(),
            url: "https://frontend-3000.brs.devtunnels.ms/".into(),
            expiration: "30d".into(),
            host_connections: 0,
        }];
        save_row_cache_to(&path, &rows);
        let loaded = load_row_cache_from(&path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].tunnel_id, "frontend.brs");
        assert_eq!(loaded[0].port, 3000);

        // Invalid content -> empty list.
        fs::write(&path, "garbage").unwrap();
        assert!(load_row_cache_from(&path).is_empty());
    }

    #[test]
    fn auto_host_mutators() {
        let mut st = AppState::default();
        st.add_auto_host("a.brs");
        st.add_auto_host("b.brs");
        st.add_auto_host("a.brs"); // dedup
        assert_eq!(st.auto_host, vec!["a.brs".to_string(), "b.brs".to_string()]);
        assert!(st.contains_auto_host("a.brs"));
        assert!(!st.contains_auto_host("c.brs"));

        st.remove_auto_host("a.brs");
        assert!(!st.contains_auto_host("a.brs"));
        assert_eq!(st.auto_host, vec!["b.brs".to_string()]);

        // Removing an absent id is a no-op.
        st.remove_auto_host("zzz");
        assert_eq!(st.auto_host, vec!["b.brs".to_string()]);
    }
}
