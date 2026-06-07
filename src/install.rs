//! Per-user install of the portable executable.
//!
//! The app ships as a single portable `.exe` (downloaded from CI). When the user
//! enables "Start with Windows" while running that portable file, we relocate it
//! into the standard no-admin per-user programs folder
//! (`%LOCALAPPDATA%\Programs\DevTunnelGUI`), create a Start-menu shortcut, point
//! auto-start at the new location, relaunch from there, and delete the original
//! portable file. Windows-only — the rest of the app's install story is moot
//! elsewhere.

#![cfg(windows)]

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Sub-folder name under `%LOCALAPPDATA%\Programs` and the Start-menu link label.
const APP_DIR_NAME: &str = "DevTunnelGUI";
/// Installed executable file name.
const EXE_NAME: &str = "devtunnel_gui.exe";
/// Friendly name shown for the Start-menu shortcut (and its `.lnk` file stem).
const SHORTCUT_NAME: &str = "DevTunnel GUI";
/// CLI flag the relocated instance receives so it can delete the portable
/// original it was launched from.
pub const RELOCATED_FROM_FLAG: &str = "--relocated-from";

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

/// Whether the Start-menu shortcut exists on disk.
pub fn shortcut_exists() -> bool {
    shortcut_path().map(|p| p.exists()).unwrap_or(false)
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

/// Copies the running executable into [`programs_dir`], returning the installed
/// path. Creates the directory if needed and overwrites any prior copy. Only
/// called when [`is_installed`] is false, so source and destination differ.
pub fn install_self() -> Result<PathBuf> {
    let src = std::env::current_exe().context("locating current executable")?;
    let dir = programs_dir().context("LOCALAPPDATA is not set")?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating install dir {}", dir.display()))?;
    let dest = dir.join(EXE_NAME);
    std::fs::copy(&src, &dest)
        .with_context(|| format!("copying {} -> {}", src.display(), dest.display()))?;
    Ok(dest)
}

/// Creates (or overwrites) the Start-menu shortcut pointing at `target`, using
/// the executable itself as the icon source.
pub fn create_start_menu_shortcut(target: &Path) -> Result<()> {
    let link = shortcut_path().context("APPDATA is not set")?;
    if let Some(parent) = link.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut sl = mslnk::ShellLink::new(target)
        .with_context(|| format!("preparing shortcut for {}", target.display()))?;
    sl.set_name(Some(SHORTCUT_NAME.to_string()));
    sl.set_icon_location(Some(target.to_string_lossy().to_string()));
    if let Some(dir) = target.parent() {
        sl.set_working_dir(Some(dir.to_string_lossy().to_string()));
    }
    sl.create_lnk(&link)
        .with_context(|| format!("writing shortcut {}", link.display()))?;
    Ok(())
}

/// Relaunches `new_exe`, passing the portable original path so the fresh instance
/// can delete it once this process exits. The caller exits after this returns Ok.
pub fn relaunch_from(new_exe: &Path, old_exe: &Path) -> Result<()> {
    std::process::Command::new(new_exe)
        .arg(RELOCATED_FROM_FLAG)
        .arg(old_exe)
        .spawn()
        .with_context(|| format!("relaunching {}", new_exe.display()))?;
    Ok(())
}

/// Deletes the portable original after relocation. The previous process needs a
/// moment to exit and release the file lock, so retry briefly. Best-effort: a
/// stubborn file left behind is harmless (auto-start already points at the copy).
pub fn cleanup_relocated(old_exe: &Path) {
    for _ in 0..20 {
        match std::fs::remove_file(old_exe) {
            Ok(()) => return,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(150)),
        }
    }
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
        let exe = Path::new(r"C:\Users\Me\AppData\Local\Programs\DevTunnelGUI-old\devtunnel_gui.exe");
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
