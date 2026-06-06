//! Start-with-Windows toggle: an entry in the per-user
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` key pointing at the
//! current executable. No elevation required (HKCU, not HKLM). The app already
//! starts minimized to the tray, so auto-start never pops a window at logon.

#![cfg(windows)]

use std::path::Path;
use winreg::enums::HKEY_CURRENT_USER;
use winreg::RegKey;

/// Registry value name under the Run key. A single constant so enable/disable
/// and is_enabled always agree.
const RUN_VALUE_NAME: &str = "DevTunnelGUI";
/// Per-user Run key (relative to HKCU).
const RUN_KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// Formats the Run-key command for an executable path: the path wrapped in
/// double quotes so spaces in the install location do not break the command.
/// Pure function, unit-tested without touching the registry.
fn run_command(exe: &Path) -> String {
    format!("\"{}\"", exe.display())
}

/// Whether the auto-start Run entry currently exists.
pub fn is_enabled() -> bool {
    RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(RUN_KEY_PATH)
        .and_then(|key| key.get_value::<String, _>(RUN_VALUE_NAME))
        .is_ok()
}

/// Writes the Run entry pointing at the current executable.
pub fn enable() -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(RUN_KEY_PATH)?;
    key.set_value(RUN_VALUE_NAME, &run_command(&exe))?;
    Ok(())
}

/// Deletes the Run entry. A missing value is treated as success (already disabled).
pub fn disable() -> anyhow::Result<()> {
    let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(
        RUN_KEY_PATH,
        winreg::enums::KEY_SET_VALUE | winreg::enums::KEY_QUERY_VALUE,
    )?;
    match key.delete_value(RUN_VALUE_NAME) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Applies the requested auto-start state: enables or disables the Run entry.
pub fn set_enabled(enabled: bool) -> anyhow::Result<()> {
    if enabled {
        enable()
    } else {
        disable()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn run_command_quotes_the_exe_path() {
        let exe = PathBuf::from(r"C:\Program Files\DevTunnel GUI\devtunnel_gui.exe");
        assert_eq!(
            run_command(&exe),
            r#""C:\Program Files\DevTunnel GUI\devtunnel_gui.exe""#
        );
    }

    #[test]
    fn run_command_quotes_simple_path_too() {
        let exe = PathBuf::from(r"C:\bin\app.exe");
        assert_eq!(run_command(&exe), r#""C:\bin\app.exe""#);
    }
}
