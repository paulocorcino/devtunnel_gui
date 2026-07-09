//! Per-user install helpers for the portable executable.
//!
//! The app ships as a single portable `.exe`. Enabling "Start with Windows" no
//! longer relocates the binary — the auto-start entry points at wherever the app
//! currently runs from (see `autostart`). What remains here is status reporting
//! (`is_installed`) and the uninstall teardown
//! (`remove_shortcut`, `spawn_self_delete`). Windows-only — the rest of the app's
//! install story is moot elsewhere.

#![cfg(windows)]

use anyhow::{Context, Result};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};

/// Process creation flag: run the detached deleter with no console window.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Sub-folder name under `%LOCALAPPDATA%\Programs` and the Start-menu link label.
const APP_DIR_NAME: &str = "DevTunnelGUI";
/// Friendly name shown for the Start-menu shortcut (and its `.lnk` file stem).
const SHORTCUT_NAME: &str = "DevTunnel GUI";

/// `%LOCALAPPDATA%\Programs\DevTunnelGUI` — the no-admin per-user install dir.
pub fn programs_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(|base| PathBuf::from(base).join("Programs").join(APP_DIR_NAME))
}

/// `%APPDATA%\Microsoft\Windows\Start Menu\Programs` — per-user Start menu.
fn start_menu_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .map(|base| PathBuf::from(base).join(r"Microsoft\Windows\Start Menu\Programs"))
}

/// Path to the app's Start-menu shortcut.
pub fn shortcut_path() -> Option<PathBuf> {
    start_menu_dir().map(|d| d.join(format!("{SHORTCUT_NAME}.lnk")))
}

/// Whether the running executable already lives in [`programs_dir`].
pub fn is_installed() -> bool {
    match (std::env::current_exe().ok(), programs_dir()) {
        (Some(exe), Some(dir)) => path_starts_with(&exe, &dir),
        _ => false,
    }
}

/// Case-insensitive path-prefix test (Windows paths ignore case and accept either
/// slash). The prefix is matched on a directory boundary, so a sibling dir like
/// `DevTunnelGUI-old` does not count as living inside `DevTunnelGUI`. Pure —
/// unit-tested without touching the filesystem.
fn path_starts_with(path: &Path, prefix: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().to_lowercase().replace('/', "\\");
    let path = norm(path);
    let mut prefix = norm(prefix);
    if !prefix.ends_with('\\') {
        prefix.push('\\');
    }
    path.starts_with(&prefix)
}

/// Deletes the Start-menu shortcut if it exists. A missing shortcut counts as
/// success (nothing to remove). Used by the in-app uninstall flow.
pub fn remove_shortcut() -> Result<()> {
    let Some(link) = shortcut_path() else {
        return Ok(());
    };
    match std::fs::remove_file(&link) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing shortcut {}", link.display())),
    }
}

/// Schedules deletion of the installed app after this process exits.
///
/// A running executable cannot delete itself, so we spawn a **detached** `cmd`
/// that waits a couple of seconds for our process to release the file lock, then
/// removes the whole per-user install directory (or just the executable when
/// running portable). The deleter's working directory is set to the temp folder
/// because the app, launched from its Start-menu shortcut, has its CWD inside
/// the install directory — which would otherwise block `rmdir`. Best-effort by
/// nature: a file left behind is harmless. The caller exits right after.
pub fn spawn_self_delete() -> Result<()> {
    let exe = std::env::current_exe().context("locating current executable")?;
    // When installed, wipe the whole install dir; otherwise just the executable.
    let (verb, target) = match (is_installed(), programs_dir()) {
        (true, Some(dir)) => ("rmdir /S /Q", dir),
        _ => ("del /F /Q", exe),
    };
    let target = target.to_string_lossy().to_string();
    // `ping -n 4` waits ~3 s without needing a console (unlike `timeout`),
    // giving this process time to exit and release the lock before the delete.
    let script = format!("ping 127.0.0.1 -n 4 > nul & {verb} \"{target}\"");
    std::process::Command::new("cmd")
        .args(["/C", &script])
        .current_dir(std::env::temp_dir())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .with_context(|| format!("scheduling self-delete of {target}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_prefix_is_case_insensitive() {
        let exe = Path::new(r"C:\Users\Me\AppData\Local\Programs\DevTunnelGUI\devtunnel_gui.exe");
        let dir = Path::new(r"c:\users\me\appdata\local\programs\devtunnelgui");
        assert!(path_starts_with(exe, dir));
    }

    #[test]
    fn path_prefix_rejects_other_dirs() {
        let exe = Path::new(r"C:\Users\Me\Downloads\devtunnel_gui.exe");
        let dir = Path::new(r"C:\Users\Me\AppData\Local\Programs\DevTunnelGUI");
        assert!(!path_starts_with(exe, dir));
    }

    #[test]
    fn path_prefix_rejects_sibling_with_extended_name() {
        // A sibling dir whose name merely starts with the install dir name must
        // not count as "installed" (regression: missing dir-boundary check).
        let exe =
            Path::new(r"C:\Users\Me\AppData\Local\Programs\DevTunnelGUI-old\devtunnel_gui.exe");
        let dir = Path::new(r"C:\Users\Me\AppData\Local\Programs\DevTunnelGUI");
        assert!(!path_starts_with(exe, dir));
    }

    #[test]
    fn path_prefix_normalizes_slashes() {
        let exe = Path::new("C:/Users/Me/AppData/Local/Programs/DevTunnelGUI/devtunnel_gui.exe");
        let dir = Path::new(r"C:\Users\Me\AppData\Local\Programs\DevTunnelGUI");
        assert!(path_starts_with(exe, dir));
    }
}
