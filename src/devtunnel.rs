//! Subprocess layer for the `devtunnel` CLI (no PowerShell).
//! Invokes the binary directly via `std::process::Command` and deserializes the `-j` output.
//! Always runs off the UI thread.

use crate::locale::Locale;
use crate::model::{ShowResult, TunnelList};
use anyhow::{anyhow, Context, Result};
use fluent_bundle::FluentArgs;
use serde::de::DeserializeOwned;
use std::process::Command;

/// A flattened port with its URL, ready for the UI. Serde derives support the
/// startup row cache (`state::save_row_cache` / `state::load_row_cache`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Row {
    /// Friendly group name (falls back to tunnel_id when the tunnel has no name).
    pub group: String,
    /// The Real Tunnel ID — the stable key used by the service.
    pub tunnel_id: String,
    pub port: i32,
    pub protocol: String,
    pub url: String,
    pub expiration: String,
    /// Active host connections reported by the service (`hostConnections` from `list -j`).
    /// Non-zero means some session is hosting this tunnel (possibly not this app instance).
    /// A live service value; `#[serde(default)]` tolerates older row-cache files that
    /// predate this field (it is refreshed by the first live `list` after startup).
    #[serde(default)]
    pub host_connections: i64,
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
        "login required",
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
    if NEEDLES.iter().any(|n| lower.contains(n)) {
        return true;
    }
    // A token reported as invalid/revoked is also an auth failure.
    lower.contains("token") && (lower.contains("invalid") || lower.contains("revoked"))
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

/// Builds the argv for re-applying a tunnel's expiration window:
/// `update <id> --expiration <exp> -j`. Pure so the renewal contract is testable.
pub fn update_expiration_args(tunnel_id: &str, expiration: &str) -> Vec<String> {
    vec![
        "update".into(),
        tunnel_id.into(),
        "--expiration".into(),
        expiration.into(),
        "-j".into(),
    ]
}

/// Builds the argv for re-creating the anonymous-access ACE:
/// `access create <id> --anonymous -j`. Pure so the renewal contract is testable.
pub fn anonymous_ace_args(tunnel_id: &str) -> Vec<String> {
    vec![
        "access".into(),
        "create".into(),
        tunnel_id.into(),
        "--anonymous".into(),
        "-j".into(),
    ]
}

/// Pure classifier for `access list -j` output: does the tunnel currently
/// grant anonymous access (an `Anonymous` entry that is not a deny rule)?
pub fn classify_anonymous_access(v: &serde_json::Value) -> bool {
    v.get("accessControlEntries")
        .and_then(|e| e.as_array())
        .is_some_and(|entries| {
            entries.iter().any(|e| {
                e.get("type").and_then(|t| t.as_str()) == Some("Anonymous")
                    && !e.get("isDeny").and_then(|d| d.as_bool()).unwrap_or(false)
            })
        })
}

/// True when the tunnel currently grants anonymous access, via
/// `access list <id> -j`. Renewal uses this so it only refreshes the ACE on
/// groups the user actually made anonymous — never widening access.
pub fn has_anonymous_access(tunnel_id: &str, loc: &Locale) -> Result<bool> {
    let v: serde_json::Value = run_json(&["access", "list", tunnel_id, "-j"], loc)?;
    Ok(classify_anonymous_access(&v))
}

/// Renews a tunnel (issue #6): re-applies the expiration window (skipped when
/// `expiration` is empty) and refreshes the anonymous ACE — but only when the
/// tunnel already grants anonymous access, so renewal never turns an
/// authenticated group public. All calls are idempotent one-shot subprocess
/// invocations, independent of any in-process SDK hosting session.
pub fn renew_tunnel(tunnel_id: &str, expiration: &str, loc: &Locale) -> Result<()> {
    let expiration = expiration.trim();
    if !expiration.is_empty() {
        let args = update_expiration_args(tunnel_id, expiration);
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        run_ok(&argv, loc)?;
    }
    if has_anonymous_access(tunnel_id, loc)? {
        let args = anonymous_ace_args(tunnel_id);
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        run_ok(&argv, loc)?;
    }
    Ok(())
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

/// Live metrics of a single port, mapped from the `status` block of `show -j`.
/// Every field is optional: `None` renders as "n/a" in the UI.
pub struct PortMetrics {
    /// Current upload rate, bytes per second.
    pub upload_rate: Option<f64>,
    /// Current download rate, bytes per second.
    pub download_rate: Option<f64>,
    /// Total bytes uploaded since hosting started.
    pub upload_total: Option<f64>,
    /// Total bytes downloaded since hosting started.
    pub download_total: Option<f64>,
    /// Active client connections.
    pub connection_count: Option<f64>,
}

/// Fetches the live metrics of one port via `show <id> -j`. Absent metrics map
/// to `None`; a port missing from the tunnel is an error (it was deleted).
pub fn fetch_port_status(tunnel_id: &str, port: i32, loc: &Locale) -> Result<PortMetrics> {
    let show: ShowResult = run_json(&["show", tunnel_id, "-j"], loc)?;
    let detail = show
        .tunnel
        .ports
        .into_iter()
        .find(|p| p.port_number == port)
        .ok_or_else(|| {
            let mut a = FluentArgs::new();
            a.set("port", port as i64);
            a.set("tunnel", tunnel_id.to_string());
            anyhow!("{}", loc.t_args("err-port-not-found", &a))
        })?;
    let s = detail.status.unwrap_or_default();
    Ok(PortMetrics {
        upload_rate: s.current_upload_rate,
        download_rate: s.current_download_rate,
        upload_total: s.upload_total,
        download_total: s.download_total,
        connection_count: s.client_connection_count,
    })
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
        let host_connections = t.host_connections;
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
                        host_connections,
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
                            host_connections,
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
                host_connections,
            }),
        }
    }

    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::{
        anonymous_ace_args, classify_anonymous_access, classify_user_show, is_auth_error,
        sanitize_tunnel_id, update_expiration_args,
    };

    #[test]
    fn anonymous_access_detected_on_allow_entry() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"accessControlEntries":[{"type":"Anonymous","subjects":[],"scopes":["connect"]}]}"#,
        )
        .unwrap();
        assert!(classify_anonymous_access(&v));
    }

    #[test]
    fn anonymous_access_false_on_deny_empty_or_other_entries() {
        let deny: serde_json::Value = serde_json::from_str(
            r#"{"accessControlEntries":[{"type":"Anonymous","isDeny":true}]}"#,
        )
        .unwrap();
        assert!(!classify_anonymous_access(&deny));

        let empty: serde_json::Value =
            serde_json::from_str(r#"{"accessControlEntries":[]}"#).unwrap();
        assert!(!classify_anonymous_access(&empty));

        let other: serde_json::Value =
            serde_json::from_str(r#"{"accessControlEntries":[{"type":"Tenant"}]}"#).unwrap();
        assert!(!classify_anonymous_access(&other));

        let missing: serde_json::Value = serde_json::from_str(r#"{}"#).unwrap();
        assert!(!classify_anonymous_access(&missing));
    }

    #[test]
    fn update_expiration_args_builds_exact_argv() {
        assert_eq!(
            update_expiration_args("frontend.brs", "30d"),
            vec!["update", "frontend.brs", "--expiration", "30d", "-j"]
        );
    }

    #[test]
    fn anonymous_ace_args_builds_exact_argv() {
        assert_eq!(
            anonymous_ace_args("frontend.brs"),
            vec!["access", "create", "frontend.brs", "--anonymous", "-j"]
        );
    }

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

    // ---- is_auth_error: representative CLI auth-failure messages ----

    #[test]
    fn auth_error_on_not_logged_in() {
        assert!(is_auth_error(
            "Not logged in. Run 'devtunnel user login' to log in."
        ));
    }

    #[test]
    fn auth_error_on_login_required_hint() {
        assert!(is_auth_error(
            "error: Login required. Please run `devtunnel user login`."
        ));
    }

    #[test]
    fn auth_error_on_unauthorized() {
        assert!(is_auth_error("The request was rejected: 401 Unauthorized."));
    }

    #[test]
    fn auth_error_on_expired_or_revoked_token() {
        assert!(is_auth_error(
            "Authentication failed: the access token has expired."
        ));
        assert!(is_auth_error("error: token is invalid or revoked"));
    }

    #[test]
    fn auth_error_matches_wrapped_localized_error() {
        // The engine sees the localized wrapper (err-cli-failed) around the raw
        // stderr; the classifier must still hit on the embedded CLI text.
        assert!(is_auth_error(
            "`devtunnel token x --scopes host -j` returned error: Not logged in. Run 'devtunnel user login'."
        ));
    }

    #[test]
    fn not_auth_error_on_other_errors() {
        assert!(!is_auth_error("connection timed out"));
        assert!(!is_auth_error(
            "tunnel id has no cluster suffix (expected 'id.cluster'): foo"
        ));
        assert!(!is_auth_error("port number must be between 1 and 65535"));
        assert!(!is_auth_error("503 Service Unavailable"));
    }
}
