//! Background check for a newer GitHub release.
//!
//! On startup and every 24 h thereafter, a background thread queries the GitHub
//! Releases API for the latest published release and compares its tag against
//! the running build's `GIT_VERSION`. When the release is strictly newer it
//! sends an `UpdateInfo` to the UI thread, which surfaces an in-app banner.
//!
//! The check is best-effort: network failures are logged at debug and retried
//! on the next tick — they never surface to the user or block the UI.

use std::sync::mpsc::Sender;
#[cfg(not(feature = "store"))]
use std::time::Duration;

/// GitHub Releases API for this repo's latest (non-prerelease) release.
#[cfg(not(feature = "store"))]
const RELEASES_API: &str =
    "https://api.github.com/repos/paulocorcino/devtunnel_gui/releases/latest";

/// Public release page, used as the click target when the API omits `html_url`.
#[cfg(not(feature = "store"))]
const RELEASES_PAGE: &str = "https://github.com/paulocorcino/devtunnel_gui/releases/latest";

/// How often to re-check after the initial startup check. The app is a tray
/// app that can stay open for days, so a one-shot startup check could never
/// fire for long-running instances.
#[cfg(not(feature = "store"))]
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// A release newer than the running build, pumped to the UI thread.
#[derive(Clone, Debug)]
// In `store` builds the checker is a no-op, so the fields are never read; the
// type is still referenced by `spawn`'s channel signature.
#[cfg_attr(feature = "store", allow(dead_code))]
pub struct UpdateInfo {
    /// The release tag, as published (e.g. `v0.2.0`).
    pub version: String,
    /// The release page to open in the browser.
    pub url: String,
}

/// Spawns the background update checker. Sends an `UpdateInfo` on the channel
/// whenever the latest release is newer than the running build, then sleeps
/// until the next check. Stops when the receiver is dropped (UI shut down).
#[cfg(feature = "store")]
pub fn spawn(_tx: Sender<UpdateInfo>) {
    // Store (MSIX) builds are updated through the Microsoft Store, not GitHub
    // Releases. Self-directed update prompts are against Store policy, so the
    // checker is compiled out entirely — the banner never fires.
}

#[cfg(not(feature = "store"))]
pub fn spawn(tx: Sender<UpdateInfo>) {
    // Test hook: force the banner without a live release. `DEVTUNNEL_FAKE_UPDATE`
    // is the tag to advertise (e.g. `v9.9.9`); the URL points at the releases
    // page. Used to verify the banner UI locally.
    if let Ok(tag) = std::env::var("DEVTUNNEL_FAKE_UPDATE") {
        let _ = tx.send(UpdateInfo {
            version: tag,
            url: RELEASES_PAGE.to_string(),
        });
        return;
    }

    let current = env!("GIT_VERSION");
    std::thread::spawn(move || loop {
        match check_latest() {
            Ok(Some(info)) if is_newer(&info.version, current) => {
                if tx.send(info).is_err() {
                    return; // Receiver gone — the UI is shutting down.
                }
            }
            Ok(_) => {}
            Err(e) => log::debug!("update check failed: {e}"),
        }
        std::thread::sleep(CHECK_INTERVAL);
    });
}

/// Queries the GitHub API for the latest release. Returns `Ok(None)` when the
/// response carries no usable tag.
#[cfg(not(feature = "store"))]
fn check_latest() -> anyhow::Result<Option<UpdateInfo>> {
    let resp = ureq::get(RELEASES_API)
        // GitHub rejects requests without a User-Agent.
        .set("User-Agent", "devtunnel_gui")
        .set("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(10))
        .call()?;
    // ureq's `json` feature is off (keeps the default build light); parse the
    // body with serde_json directly.
    let json: serde_json::Value = serde_json::from_str(&resp.into_string()?)?;
    let tag = json.get("tag_name").and_then(|v| v.as_str()).unwrap_or("");
    if tag.is_empty() {
        return Ok(None);
    }
    let url = json
        .get("html_url")
        .and_then(|v| v.as_str())
        .unwrap_or(RELEASES_PAGE)
        .to_string();
    Ok(Some(UpdateInfo {
        version: tag.to_string(),
        url,
    }))
}

/// Returns true when `candidate` is a strictly newer semantic version than
/// `current`. Both may carry a leading `v` and `-`/`+` build suffixes
/// (e.g. `v0.2.0`, `0.1.0+g05b8b3c-dirty`); only MAJOR.MINOR.PATCH is compared.
/// Anything that cannot be parsed is treated as not-newer (fail closed), so a
/// malformed tag never triggers a spurious "update available".
#[cfg(not(feature = "store"))]
fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse_semver(candidate), parse_semver(current)) {
        (Some(c), Some(cur)) => c > cur,
        _ => false,
    }
}

/// Extracts `(major, minor, patch)` from a version string, ignoring a leading
/// `v` and any `-`/`+` suffix. Missing minor/patch default to 0. Returns `None`
/// if the numeric core is absent or non-numeric.
#[cfg(not(feature = "store"))]
fn parse_semver(s: &str) -> Option<(u64, u64, u64)> {
    let core = s.trim().trim_start_matches(['v', 'V']);
    let core = core.split(['-', '+']).next().unwrap_or("");
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor, patch))
}

#[cfg(all(test, not(feature = "store")))]
mod tests {
    use super::*;

    #[test]
    fn newer_detects_bump() {
        assert!(is_newer("v0.2.0", "0.1.0+g05b8b3c"));
        assert!(is_newer("v0.1.1", "v0.1.0"));
        assert!(is_newer("1.0.0", "v0.9.9"));
    }

    #[test]
    fn not_newer_when_equal_or_older() {
        // Untagged dev build of the same release must not self-notify.
        assert!(!is_newer("v0.1.0", "0.1.0+g05b8b3c"));
        assert!(!is_newer("v0.1.0", "v0.2.0"));
        // Commits past the tag on the same MAJOR.MINOR.PATCH are not newer.
        assert!(!is_newer("v0.2.0", "v0.2.0-3-gabc1234"));
    }

    #[test]
    fn unparseable_is_not_newer() {
        assert!(!is_newer("nightly", "v0.1.0"));
        assert!(!is_newer("v0.2.0", "not-a-version"));
    }

    #[test]
    fn parses_suffixes() {
        assert_eq!(parse_semver("v0.2.0-3-gabc1234"), Some((0, 2, 0)));
        assert_eq!(parse_semver("0.1.0+g05b8b3c-dirty"), Some((0, 1, 0)));
        assert_eq!(parse_semver("v1.2"), Some((1, 2, 0)));
    }
}
