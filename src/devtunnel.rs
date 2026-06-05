//! Subprocess layer for the `devtunnel` CLI (no PowerShell).
//! Invokes the binary directly via `std::process::Command` and deserializes the `-j` output.
//! Always runs off the UI thread.

use crate::locale::Locale;
use crate::model::{ShowResult, TunnelList};
use anyhow::{anyhow, Context, Result};
use fluent_bundle::FluentArgs;
use serde::de::DeserializeOwned;
use std::process::Command;

/// A flattened port with its URL, ready for the UI.
pub struct Row {
    /// Friendly group name (falls back to tunnel_id when the tunnel has no name).
    pub group: String,
    /// The Real Tunnel ID — the stable key used by the service.
    pub tunnel_id: String,
    pub port: i32,
    pub protocol: String,
    pub url: String,
    pub expiration: String,
}

/// Resolves the binary. Allows override via `DEVTUNNEL_BIN`; otherwise trusts PATH.
fn bin() -> String {
    std::env::var("DEVTUNNEL_BIN").unwrap_or_else(|_| "devtunnel".to_string())
}

/// Result of the startup preflight: is the CLI present and logged in?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preflight {
    /// CLI found and a user is logged in.
    Ok,
    /// The `devtunnel` binary could not be executed (not on PATH).
    CliMissing,
    /// CLI present but no valid login (never logged in or token expired).
    LoggedOut,
}

/// Probes the environment: `devtunnel --version` for CLI presence, then
/// `devtunnel user show -j` for login state. Never errors — the outcome is the
/// enum, which the UI maps to a banner state.
pub fn preflight() -> Preflight {
    if Command::new(bin()).arg("--version").output().is_err() {
        return Preflight::CliMissing;
    }
    match Command::new(bin()).args(["user", "show", "-j"]).output() {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            if classify_user_show(out.status.success(), &stdout) {
                Preflight::Ok
            } else {
                Preflight::LoggedOut
            }
        }
        Err(_) => Preflight::CliMissing,
    }
}

/// Pure decision logic for [`preflight`]'s login probe: classifies the result
/// of `devtunnel user show -j` as logged-in (`true`) or logged-out (`false`).
/// A failed command or an output reporting "not logged in" means logged out.
pub fn classify_user_show(success: bool, stdout: &str) -> bool {
    success && !stdout.to_ascii_lowercase().contains("not logged in")
}

/// Heuristically classifies a CLI/host error message as an authentication /
/// login-expiry failure (as opposed to a generic CLI error). Drives the switch
/// into the re-login state when hosting or management fails mid-session.
pub fn is_auth_error(stderr: &str) -> bool {
    const NEEDLES: &[&str] = &[
        "unauthorized",
        "not logged in",
        "login expired",
        "login has expired",
        "authentication failed",
        "authentication required",
        "token is expired",
        "token has expired",
        "devtunnel user login",
        "please log in",
        "401",
        "403",
    ];
    let lower = stderr.to_ascii_lowercase();
    NEEDLES.iter().any(|n| lower.contains(n))
}

/// Runs `devtunnel user login` (interactive — opens the system browser) and
/// waits for it to finish. The caller re-runs [`preflight`] afterwards to
/// confirm the login took effect.
pub fn user_login(loc: &Locale) -> Result<()> {
    run_ok(&["user", "login"], loc)
}

/// Options for creating a group (tunnel). Mirrors the minimal + advanced fields
/// of the "New group" dialog.
pub struct CreateGroupOpts {
    /// Friendly group name. Sanitized to a valid Tunnel ID before use.
    pub name: String,
    /// Expiration string accepted by the CLI (e.g. `30d`, `12h`). Empty = CLI default.
    pub expiration: String,
    /// Apply an anonymous-access ACE so the Public URLs are reachable without login.
    pub anonymous: bool,
    pub description: String,
    /// Advanced: keep the original Host/Origin headers (`--host-header/--origin-header unchanged`).
    pub keep_headers: bool,
    /// Advanced: web request timeout in seconds (0 = disabled). Empty = CLI default.
    pub request_timeout: String,
}

/// Options for adding a port to an existing group (tunnel).
pub struct CreatePortOpts {
    pub port: i32,
    /// `http`, `https`, or `auto`.
    pub protocol: String,
    pub description: String,
    pub keep_headers: bool,
    pub request_timeout: String,
}

/// Sanitizes a free-form group name into a valid Dev Tunnel ID: lowercase ASCII
/// alphanumerics and single hyphens, no leading/trailing hyphen. Spaces and
/// underscores collapse to a single hyphen; other characters are dropped.
/// The service still validates length and format — invalid results surface as a CLI error.
pub fn sanitize_tunnel_id(name: &str) -> String {
    let mut out = String::new();
    let mut prev_hyphen = false;
    for c in name.trim().chars() {
        let lc = c.to_ascii_lowercase();
        if lc.is_ascii_alphanumeric() {
            out.push(lc);
            prev_hyphen = false;
        } else if matches!(lc, '-' | ' ' | '_') && !out.is_empty() && !prev_hyphen {
            out.push('-');
            prev_hyphen = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Runs a mutating CLI command, checking only the exit status (output ignored).
/// Used for operations whose JSON payload the UI does not consume.
fn run_ok(args: &[&str], loc: &Locale) -> Result<()> {
    let joined = args.join(" ");
    let output = Command::new(bin()).args(args).output().with_context(|| {
        let mut a = FluentArgs::new();
        a.set("args", joined.clone());
        loc.t_args("err-cli-not-found", &a)
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let mut a = FluentArgs::new();
        a.set("args", joined.clone());
        a.set("stderr", stderr.trim().to_string());
        return Err(anyhow!("{}", loc.t_args("err-cli-failed", &a)));
    }
    Ok(())
}

/// Creates a group (tunnel) and returns its Real Tunnel ID. Applies an anonymous
/// ACE in the same call when requested. The subsequent list refresh reconciles the UI.
pub fn create_group(opts: &CreateGroupOpts, loc: &Locale) -> Result<String> {
    let id = sanitize_tunnel_id(&opts.name);
    if id.is_empty() {
        return Err(anyhow!("{}", loc.t("err-empty-group-name")));
    }

    let mut args: Vec<String> = vec!["create".into(), id.clone()];
    if opts.anonymous {
        args.push("-a".into());
    }
    if !opts.expiration.trim().is_empty() {
        args.push("-e".into());
        args.push(opts.expiration.trim().into());
    }
    if !opts.description.trim().is_empty() {
        args.push("-d".into());
        args.push(opts.description.trim().into());
    }
    if opts.keep_headers {
        args.push("--host-header".into());
        args.push("unchanged".into());
        args.push("--origin-header".into());
        args.push("unchanged".into());
    }
    if !opts.request_timeout.trim().is_empty() {
        args.push("--request-timeout".into());
        args.push(opts.request_timeout.trim().into());
    }
    args.push("-j".into());

    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let created: ShowResult = run_json(&argv, loc)?;
    Ok(created.tunnel.tunnel_id)
}

/// Adds a port to an existing group (tunnel).
pub fn create_port(tunnel_id: &str, opts: &CreatePortOpts, loc: &Locale) -> Result<()> {
    if !(1..=65535).contains(&opts.port) {
        return Err(anyhow!("{}", loc.t("err-invalid-port")));
    }
    let port = opts.port.to_string();
    let mut args: Vec<String> = vec![
        "port".into(),
        "create".into(),
        tunnel_id.into(),
        "-p".into(),
        port,
    ];
    if !opts.protocol.trim().is_empty() {
        args.push("--protocol".into());
        args.push(opts.protocol.trim().into());
    }
    if !opts.description.trim().is_empty() {
        args.push("-d".into());
        args.push(opts.description.trim().into());
    }
    if opts.keep_headers {
        args.push("--host-header".into());
        args.push("unchanged".into());
        args.push("--origin-header".into());
        args.push("unchanged".into());
    }
    if !opts.request_timeout.trim().is_empty() {
        args.push("--request-timeout".into());
        args.push(opts.request_timeout.trim().into());
    }
    args.push("-j".into());

    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    run_ok(&argv, loc)
}

/// Deletes an entire group (tunnel) and all of its ports.
pub fn delete_group(tunnel_id: &str, loc: &Locale) -> Result<()> {
    run_ok(&["delete", tunnel_id, "-f", "-j"], loc)
}

/// Deletes a single port from a group, leaving the others untouched.
pub fn delete_port(tunnel_id: &str, port: i32, loc: &Locale) -> Result<()> {
    let port = port.to_string();
    run_ok(&["port", "delete", tunnel_id, "-p", &port, "-j"], loc)
}

/// Mints an access token for a tunnel via `devtunnel token <id> --scopes <scope> -j`,
/// returning the raw token string. Mint one scope at a time: repeating `--scopes`
/// on the CLI corrupts the first value. Used by the host engine (`host` scope to
/// connect to the relay, `manage:ports` so the SDK can create ports).
#[cfg_attr(not(feature = "hosting"), allow(dead_code))]
pub fn mint_token(full_id: &str, scope: &str, loc: &Locale) -> Result<String> {
    let args = ["token", full_id, "--scopes", scope, "-j"];
    let joined = args.join(" ");
    let output = Command::new(bin()).args(args).output().with_context(|| {
        let mut a = FluentArgs::new();
        a.set("args", joined.clone());
        loc.t_args("err-cli-not-found", &a)
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let mut a = FluentArgs::new();
        a.set("args", joined.clone());
        a.set("stderr", stderr.trim().to_string());
        return Err(anyhow!("{}", loc.t_args("err-cli-failed", &a)));
    }

    let v: serde_json::Value = serde_json::from_slice(&output.stdout).with_context(|| {
        let mut a = FluentArgs::new();
        a.set("args", joined.clone());
        loc.t_args("err-cli-invalid-json", &a)
    })?;
    v.get("token")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            let mut a = FluentArgs::new();
            a.set("args", joined.clone());
            anyhow!("{}", loc.t_args("err-cli-invalid-json", &a))
        })
}

/// Splits a full Tunnel ID of the form `id.cluster` (e.g. `frontend-3000.brs`)
/// into `(cluster, id)`, the shape the SDK `TunnelLocator::ID` expects. Splits at
/// the last `.`. Returns `None` when no cluster suffix is present.
#[cfg_attr(not(feature = "hosting"), allow(dead_code))]
pub fn split_locator(full_id: &str) -> Option<(String, String)> {
    full_id
        .rsplit_once('.')
        .map(|(id, cluster)| (cluster.to_string(), id.to_string()))
}

fn run_json<T: DeserializeOwned>(args: &[&str], loc: &Locale) -> Result<T> {
    let joined = args.join(" ");
    let output = Command::new(bin()).args(args).output().with_context(|| {
        let mut a = FluentArgs::new();
        a.set("args", joined.clone());
        loc.t_args("err-cli-not-found", &a)
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let mut a = FluentArgs::new();
        a.set("args", joined.clone());
        a.set("stderr", stderr.trim().to_string());
        return Err(anyhow!("{}", loc.t_args("err-cli-failed", &a)));
    }

    // The CLI can wrap its JSON in extra text (e.g. an intermittent "update
    // available" notice or a banner), which made parsing the whole stdout fail
    // transiently — typically right after a mutating command. Locate the first
    // JSON value (`{`/`[`) and parse just that, ignoring any leading/trailing noise.
    let stdout = &output.stdout;
    let start = stdout
        .iter()
        .position(|&b| b == b'{' || b == b'[')
        .unwrap_or(0);
    let mut stream = serde_json::Deserializer::from_slice(&stdout[start..]).into_iter::<T>();
    match stream.next() {
        Some(Ok(value)) => Ok(value),
        _ => {
            let mut a = FluentArgs::new();
            a.set("args", joined.clone());
            Err(anyhow!("{}", loc.t_args("err-cli-invalid-json", &a)))
        }
    }
}

/// Enumerates tunnels (`list -j`) and, for each one, fetches ports + URLs (`show -j`).
pub fn fetch_rows(loc: &Locale) -> Result<Vec<Row>> {
    let list: TunnelList = run_json(&["list", "-j"], loc)?;

    let mut rows = Vec::new();
    for t in list.tunnels {
        // Use friendly name when available; Real Tunnel ID is always the stable key.
        let group = if t.name.is_empty() {
            t.tunnel_id.clone()
        } else {
            t.name.clone()
        };
        match run_json::<ShowResult>(&["show", &t.tunnel_id, "-j"], loc) {
            Ok(show) => {
                let exp = show.tunnel.tunnel_expiration;
                if show.tunnel.ports.is_empty() {
                    rows.push(Row {
                        group: group.clone(),
                        tunnel_id: t.tunnel_id.clone(),
                        port: 0,
                        protocol: String::new(),
                        url: String::new(),
                        expiration: exp,
                    });
                } else {
                    for p in show.tunnel.ports {
                        rows.push(Row {
                            group: group.clone(),
                            tunnel_id: t.tunnel_id.clone(),
                            port: p.port_number,
                            protocol: p.protocol,
                            url: p.port_uri.unwrap_or_default(),
                            expiration: exp.clone(),
                        });
                    }
                }
            }
            // If `show` fails for a tunnel, still show the group with what we have.
            Err(_) => rows.push(Row {
                group: group.clone(),
                tunnel_id: t.tunnel_id.clone(),
                port: 0,
                protocol: String::new(),
                url: String::new(),
                expiration: t.tunnel_expiration,
            }),
        }
    }

    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::{classify_user_show, is_auth_error, sanitize_tunnel_id};

    #[test]
    fn auth_errors_are_classified_true() {
        assert!(is_auth_error("error: unauthorized."));
        assert!(is_auth_error(
            "User is not logged in. Run `devtunnel user login`."
        ));
        assert!(is_auth_error("Login expired, please sign in again"));
        assert!(is_auth_error("Authentication failed for the current user"));
        assert!(is_auth_error("The access token is expired"));
        assert!(is_auth_error("HTTP 401 Unauthorized"));
        assert!(is_auth_error("Forbidden (403)"));
    }

    #[test]
    fn generic_cli_errors_are_classified_false() {
        assert!(!is_auth_error("tunnel 'frontend-3000' not found"));
        assert!(!is_auth_error("port number must be between 1 and 65535"));
        assert!(!is_auth_error("network error: connection timed out"));
        assert!(!is_auth_error("invalid JSON from `devtunnel list -j`"));
        assert!(!is_auth_error(""));
    }

    #[test]
    fn user_show_logged_in_when_success_and_no_marker() {
        assert!(classify_user_show(
            true,
            r#"{"status":"Logged in as user@example.com"}"#
        ));
    }

    #[test]
    fn user_show_logged_out_on_marker_or_failure() {
        assert!(!classify_user_show(true, r#"{"status":"Not logged in"}"#));
        assert!(!classify_user_show(true, "NOT LOGGED IN."));
        assert!(!classify_user_show(false, r#"{"status":"Logged in"}"#));
    }

    #[test]
    fn lowercases_and_keeps_alphanumerics() {
        assert_eq!(sanitize_tunnel_id("Frontend3000"), "frontend3000");
    }

    #[test]
    fn collapses_spaces_and_underscores_to_single_hyphen() {
        assert_eq!(sanitize_tunnel_id("my  cool_app"), "my-cool-app");
    }

    #[test]
    fn drops_disallowed_characters() {
        assert_eq!(sanitize_tunnel_id("api@v2!#"), "apiv2");
    }

    #[test]
    fn trims_leading_and_trailing_separators() {
        assert_eq!(sanitize_tunnel_id("  --frontend--  "), "frontend");
    }

    #[test]
    fn empty_when_no_valid_chars() {
        assert_eq!(sanitize_tunnel_id("@@@"), "");
        assert_eq!(sanitize_tunnel_id("   "), "");
    }
}
