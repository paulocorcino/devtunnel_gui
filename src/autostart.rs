use std::env;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "DevTunnel GUI";

/// Returns the quoted command string used as the registry value.
/// Quoting is required when the exe path contains spaces.
fn run_value() -> anyhow::Result<String> {
    let exe = env::current_exe()?;
    let path = exe.to_string_lossy();
    Ok(format!("\"{path}\""))
}

/// Returns `true` if the `DevTunnel GUI` Run entry exists and points to the
/// current executable (path match ignoring quoting differences).
pub fn is_enabled() -> bool {
    #[cfg(windows)]
    {
        use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
        use winreg::RegKey;
        let Ok(hkcu) = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_KEY, KEY_READ)
        else {
            return false;
        };
        let Ok(val): Result<String, _> = hkcu.get_value(VALUE_NAME) else {
            return false;
        };
        // Compare the stored path to the current exe (strip surrounding quotes for comparison).
        let stored = val.trim_matches('"');
        env::current_exe()
            .map(|p| p.to_string_lossy().eq_ignore_ascii_case(stored))
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Adds or removes the autostart Run entry for this executable.
pub fn set_enabled(enabled: bool) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
        use winreg::RegKey;
        let hkcu = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE)
            .map_err(|e| anyhow::anyhow!("cannot open Run key: {e}"))?;
        if enabled {
            let value = run_value()?;
            hkcu.set_value(VALUE_NAME, &value)
                .map_err(|e| anyhow::anyhow!("cannot write Run value: {e}"))?;
        } else {
            match hkcu.delete_value(VALUE_NAME) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(anyhow::anyhow!("cannot remove Run value: {e}")),
            }
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = enabled;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::run_value;

    #[test]
    fn run_value_is_quoted() {
        let v = run_value().expect("current_exe must succeed in tests");
        assert!(v.starts_with('"'), "value should start with a quote: {v}");
        assert!(v.ends_with('"'), "value should end with a quote: {v}");
        // The content between quotes must be the exe path — no embedded quotes.
        let inner = &v[1..v.len() - 1];
        assert!(!inner.is_empty(), "quoted path must not be empty");
        assert!(!inner.contains('"'), "inner path must not contain quotes");
    }
}
