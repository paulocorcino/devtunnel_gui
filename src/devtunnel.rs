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

/// Process creation flag that suppresses the console window Windows would
/// otherwise flash for each subprocess.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Process creation flag that gives an interactive subprocess its own visible
/// console window (and lets it inherit stdio). Used only for commands that
/// prompt the user — notably `user login`, which must NOT be hidden.
#[cfg(windows)]
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

/// Builds a `Command` for `program` with the console window suppressed on
/// Windows. Every *silent* subprocess in this module must go through here:
/// without the flag, each one-shot `devtunnel`/`winget` call flashes a black
/// console window, which both looks broken and makes the app resemble a malware
/// installer. Interactive commands must use [`interactive_command`] instead.
fn command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Builds a `Command` for an *interactive* `devtunnel` invocation. Unlike
/// [`command`], it gives the process its own visible console (`CREATE_NEW_CONSOLE`)
/// and leaves stdio inherited, so the user can see and complete a browser /
/// device-code login. Routing `user login` through the silent [`command`] path
/// (hidden window, captured stdio) is what leaves the app frozen on
/// "signing in…" with no way to authenticate.
fn interactive_command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NEW_CONSOLE);
    }
    cmd
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
    if command(&bin()).arg("--version").output().is_err() {
        return Preflight::CliMissing;
    }
    match command(&bin()).args(["user", "show", "-j"]).output() {
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

/// Returns the logged-in account identifier from `devtunnel user show -j`
/// (the `username` field, e.g. an email), or `None` when logged out or the
/// command/parse fails. Best-effort and read-only; safe to call off the UI
/// thread to populate the Settings "Signed in as …" label.
pub fn current_username() -> Option<String> {
    let out = command(&bin()).args(["user", "show", "-j"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let start = stdout.find(['{', '['])?;
    let value: serde_json::Value = serde_json::from_str(stdout[start..].trim()).ok()?;
    value
        .get("username")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

/// Official Dev Tunnels CLI install page — opened as a fallback when `winget`
/// itself is unavailable, so the user can install the CLI manually.
pub const CLI_INSTALL_URL: &str =
    "https://learn.microsoft.com/azure/developer/dev-tunnels/get-started";

/// Outcome of an attempt to install the Dev Tunnels CLI via `winget`.
///
/// Distinguishes the four cases the onboarding UI must surface differently:
/// success, `winget` missing (fall back to the manual download page), an
/// elevation/administrator requirement (a specific, actionable failure), and any
/// other failure (carrying the trimmed `winget` stderr for display).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    /// `winget` installed the CLI successfully.
    Installed,
    /// `winget` itself is not available — the caller should open [`CLI_INSTALL_URL`].
    WingetMissing,
    /// The install needs administrator/elevated privileges that the current
    /// (non-elevated) process lacks.
    Elevation,
    /// `winget` ran but failed for some other reason; carries the trimmed stderr.
    Failed(String),
}

/// Windows `ERROR_ELEVATION_REQUIRED` — the exit code a process gets when an
/// operation needs administrator rights it does not have. Best-effort: `winget`
/// usually surfaces its own HRESULT-style codes instead, so in practice the
/// stderr substring heuristics below are what catch the elevation case; this
/// constant is a cheap extra signal, not the primary detector.
const ELEVATION_EXIT_CODE: i32 = 740;

/// Pure classifier for a finished `winget install` run: maps the success flag,
/// process exit code, and stderr to an [`InstallOutcome`]. An elevation hint in
/// the stderr ("elevat", "administrator", "requires admin") or the Windows
/// elevation exit code maps to [`InstallOutcome::Elevation`]; any other non-zero
/// result maps to [`InstallOutcome::Failed`] with the trimmed stderr; success
/// maps to [`InstallOutcome::Installed`]. Kept pure so it is unit-testable
/// without spawning `winget`.
pub fn classify_install_result(
    success: bool,
    exit_code: Option<i32>,
    stderr: &str,
) -> InstallOutcome {
    if success {
        return InstallOutcome::Installed;
    }
    let lower = stderr.to_ascii_lowercase();
    let needs_elevation = lower.contains("elevat")
        || lower.contains("administrator")
        || lower.contains("requires admin")
        || exit_code == Some(ELEVATION_EXIT_CODE);
    if needs_elevation {
        InstallOutcome::Elevation
    } else {
        InstallOutcome::Failed(stderr.trim().to_string())
    }
}

/// Attempts to install the Dev Tunnels CLI via `winget`, returning a classified
/// [`InstallOutcome`]. A spawn error (winget not on PATH) maps to
/// [`InstallOutcome::WingetMissing`]; a finished process is routed through
/// [`classify_install_result`]. Runs off the UI thread.
pub fn install_cli() -> InstallOutcome {
    let out = command("winget")
        .args([
            "install",
            "-e",
            "--id",
            "Microsoft.devtunnel",
            "--accept-package-agreements",
            "--accept-source-agreements",
        ])
        .output();
    match out {
        // winget not found on PATH — signal the caller to open the download page.
        Err(_) => InstallOutcome::WingetMissing,
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            classify_install_result(o.status.success(), o.status.code(), &stderr)
        }
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

/// Classifies a host connect/port-forward error as **non-recoverable**: retrying
/// with the same inputs can never succeed, so the engine should surface an error
/// and stop instead of looping the reconnect/backoff forever (each cycle re-mints
/// two tokens and re-runs the relay handshake against the service).
///
/// A `400 Bad Request` from the tunnel management API is a request-validation
/// failure — e.g. `add_port` rejected with "the tunnel port protocol cannot be
/// changed" when the forwarded protocol disagrees with the registered one. These
/// are permanent for identical inputs. A deleted or expired tunnel surfaces as
/// "Tunnel not found" / `404` while minting tokens; retrying that re-mint can
/// never succeed, so it must stop instead of spinning the reconnect loop forever
/// stuck on the `Authorizing` phase. Auth failures are handled separately by
/// [`is_auth_error`] (they have a recovery path: re-login), so callers should
/// check that first.
#[cfg_attr(not(feature = "hosting"), allow(dead_code))]
pub fn is_fatal_connect_error(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("400 bad request")
        || lower.contains("cannot be changed")
        || lower.contains("invalid arguments")
        || is_missing_tunnel_error(stderr)
}

/// Whether a host error means the tunnel itself no longer exists — `devtunnel
/// token` reports "Tunnel not found" / a `404` for a deleted or expired tunnel.
/// A strict subset of [`is_fatal_connect_error`]: the group should additionally
/// be dropped from the persisted auto-host set, since re-hosting it on the next
/// launch can never succeed.
#[cfg_attr(not(feature = "hosting"), allow(dead_code))]
pub fn is_missing_tunnel_error(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("not found") || lower.contains("404")
}

/// Runs `devtunnel user login` (interactive — opens the system browser and may
/// show a device code) in its own visible console and waits for it to finish.
/// Goes through [`interactive_command`] with inherited stdio — never the silent
/// [`command`] path, which would hide the auth prompt. The caller re-runs
/// [`preflight`] afterwards to confirm the login took effect.
pub fn user_login(loc: &Locale) -> Result<()> {
    let status = interactive_command(&bin())
        .args(["user", "login"])
        .status()
        .with_context(|| {
            let mut a = FluentArgs::new();
            a.set("args", "user login".to_string());
            loc.t_args("err-cli-not-found", &a)
        })?;

    if !status.success() {
        // stdio is inherited (shown in the console), so there is no captured
        // stderr to surface — report a generic login failure.
        return Err(anyhow!("{}", loc.t("err-login-failed")));
    }
    Ok(())
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
    let output = command(&bin()).args(args).output().with_context(|| {
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
    let output = command(&bin()).args(args).output().with_context(|| {
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
    let output = command(&bin()).args(args).output().with_context(|| {
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
///
/// The service reports traffic at the **tunnel** level as human-readable strings
/// (`uploadTotal: "4402 KB"`, `currentUploadRate: "0 MB/s (limit: 20 MB/s)"`),
/// not as a numeric per-port `status` object — the port `status` is just a
/// summary string like `"4 client connections"`. We therefore parse the
/// tunnel-level strings back into bytes / bytes-per-second and read the
/// connection count from the port's status string (falling back to the
/// tunnel-level `clientConnections` number).
pub fn fetch_port_status(tunnel_id: &str, port: i32, loc: &Locale) -> Result<PortMetrics> {
    let raw: serde_json::Value = run_json(&["show", tunnel_id, "-j"], loc)?;
    let tunnel = raw.get("tunnel");

    // Locate the port — its absence means it was deleted.
    let port_val = tunnel
        .and_then(|t| t.get("ports"))
        .and_then(|p| p.as_array())
        .and_then(|ports| {
            ports
                .iter()
                .find(|p| p.get("portNumber").and_then(|n| n.as_i64()) == Some(port as i64))
        });
    let Some(port_val) = port_val else {
        let mut a = FluentArgs::new();
        a.set("port", port as i64);
        a.set("tunnel", tunnel_id.to_string());
        return Err(anyhow!("{}", loc.t_args("err-port-not-found", &a)));
    };
    let port_status = port_val.get("status").and_then(|v| v.as_str());

    if log::log_enabled!(log::Level::Debug) {
        log::debug!(
            "port metrics[{tunnel_id}:{port}] up={:?} down={:?} upTotal={:?} downTotal={:?} status={:?}",
            tunnel.and_then(|t| t.get("currentUploadRate")).and_then(|v| v.as_str()),
            tunnel.and_then(|t| t.get("currentDownloadRate")).and_then(|v| v.as_str()),
            tunnel.and_then(|t| t.get("uploadTotal")).and_then(|v| v.as_str()),
            tunnel.and_then(|t| t.get("downloadTotal")).and_then(|v| v.as_str()),
            port_status,
        );
    }

    let tstr = |k: &str| tunnel.and_then(|t| t.get(k)).and_then(|v| v.as_str());
    Ok(PortMetrics {
        upload_rate: tstr("currentUploadRate").and_then(parse_rate_bps),
        download_rate: tstr("currentDownloadRate").and_then(parse_rate_bps),
        upload_total: tstr("uploadTotal").and_then(parse_size_bytes),
        download_total: tstr("downloadTotal").and_then(parse_size_bytes),
        connection_count: port_status.and_then(parse_leading_int).or_else(|| {
            tunnel
                .and_then(|t| t.get("clientConnections"))
                .and_then(|v| v.as_f64())
        }),
    })
}

/// Parses a size string such as `"4402 KB"`, `"1.2 MB"`, or (comma-locale)
/// `"1,2 MB"` into bytes. Units are 1024-based (B/KB/MB/GB/TB). Returns `None`
/// for an unrecognized shape.
fn parse_size_bytes(s: &str) -> Option<f64> {
    let s = s.trim();
    let unit_pos = s.find(|c: char| c.is_ascii_alphabetic())?;
    let (num, unit) = s.split_at(unit_pos);
    let value: f64 = num.trim().replace(',', ".").parse().ok()?;
    let mult = match unit.trim().to_ascii_uppercase().as_str() {
        "B" => 1.0,
        "KB" => 1024.0,
        "MB" => 1024.0 * 1024.0,
        "GB" => 1024.0 * 1024.0 * 1024.0,
        "TB" => 1024.0_f64.powi(4),
        _ => return None,
    };
    Some(value * mult)
}

/// Parses a rate string such as `"0 MB/s (limit: 20 MB/s)"` into bytes/second,
/// ignoring the parenthetical limit and the trailing `/s`.
fn parse_rate_bps(s: &str) -> Option<f64> {
    let head = s.split('(').next().unwrap_or(s).trim();
    let head = head.trim_end_matches("/s").trim_end_matches("/S").trim();
    parse_size_bytes(head)
}

/// Parses a leading integer from a string like `"4 client connections"`.
fn parse_leading_int(s: &str) -> Option<f64> {
    let digits: String = s
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
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

/// Fetches the ports of a single tunnel via `devtunnel show <id> -j`, each paired
/// with its configured protocol. Targeted single-subprocess lookup: unlike
/// [`fetch_rows`], it does not enumerate the whole account (`list` + a `show` per
/// tunnel), so hosting one tunnel costs one CLI round-trip regardless of how many
/// tunnels the account holds (issue #44). The protocol is carried through because
/// re-registering a port under a different protocol is rejected by the service
/// and would block hosting (issue #36).
///
/// # Errors
/// Propagates the CLI/JSON failure from the underlying `show` call.
#[cfg_attr(not(feature = "hosting"), allow(dead_code))]
pub fn fetch_tunnel_ports(tunnel_id: &str, loc: &Locale) -> Result<Vec<(u16, String)>> {
    let show: ShowResult = run_json(&["show", tunnel_id, "-j"], loc)?;
    Ok(tunnel_ports(show))
}

/// Maps a `show -j` result to `(port, protocol)` pairs, dropping ports that are
/// absent (`0`) or outside the valid `u16` range. Pure: split out from
/// [`fetch_tunnel_ports`] so the mapping is unit-tested without the CLI.
#[cfg_attr(not(feature = "hosting"), allow(dead_code))]
fn tunnel_ports(show: ShowResult) -> Vec<(u16, String)> {
    show.tunnel
        .ports
        .into_iter()
        .filter(|p| p.port_number > 0)
        .filter_map(|p| u16::try_from(p.port_number).ok().map(|n| (n, p.protocol)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        anonymous_ace_args, classify_anonymous_access, classify_install_result, classify_user_show,
        is_auth_error, is_fatal_connect_error, is_missing_tunnel_error, parse_leading_int,
        parse_rate_bps, parse_size_bytes, sanitize_tunnel_id,
        tunnel_ports, update_expiration_args, InstallOutcome, ShowResult,
    };

    #[test]
    fn tunnel_ports_filters_zero_and_preserves_protocol() {
        // `show -j` of one tunnel: a plain-http port, an https port, and an
        // unconfigured (`0`) entry that must be dropped.
        let json = r#"{ "tunnel": { "tunnelId": "x", "ports": [
            { "portNumber": 3000, "protocol": "http" },
            { "portNumber": 8443, "protocol": "https" },
            { "portNumber": 0, "protocol": "auto" }
        ] } }"#;
        let show: ShowResult = serde_json::from_str(json).expect("valid show JSON");
        assert_eq!(
            tunnel_ports(show),
            vec![
                (3000u16, "http".to_string()),
                (8443u16, "https".to_string())
            ]
        );
    }

    #[test]
    fn parse_size_bytes_handles_units_and_locales() {
        assert_eq!(parse_size_bytes("4402 KB"), Some(4402.0 * 1024.0));
        assert_eq!(parse_size_bytes("1.5 MB"), Some(1.5 * 1024.0 * 1024.0));
        // Comma decimal (the CLI localizes numbers, e.g. "28,9 days").
        assert_eq!(parse_size_bytes("1,5 MB"), Some(1.5 * 1024.0 * 1024.0));
        assert_eq!(parse_size_bytes("512 B"), Some(512.0));
        assert_eq!(parse_size_bytes("nonsense"), None);
    }

    #[test]
    fn parse_rate_bps_ignores_limit_and_suffix() {
        assert_eq!(parse_rate_bps("0 MB/s (limit: 20 MB/s)"), Some(0.0));
        assert_eq!(
            parse_rate_bps("2.5 MB/s (limit: 20 MB/s)"),
            Some(2.5 * 1024.0 * 1024.0)
        );
        assert_eq!(parse_rate_bps("128 KB/s"), Some(128.0 * 1024.0));
    }

    #[test]
    fn parse_leading_int_reads_connection_count() {
        assert_eq!(parse_leading_int("4 client connections"), Some(4.0));
        assert_eq!(parse_leading_int("0 client connections"), Some(0.0));
        assert_eq!(parse_leading_int("no number"), None);
    }

    #[test]
    fn install_elevation_detected_from_stderr_or_exit_code() {
        assert_eq!(
            classify_install_result(false, Some(1), "Access denied: elevation required"),
            InstallOutcome::Elevation
        );
        assert_eq!(
            classify_install_result(false, Some(1), "This action requires administrator rights"),
            InstallOutcome::Elevation
        );
        assert_eq!(
            classify_install_result(false, Some(740), "permission denied"),
            InstallOutcome::Elevation
        );
    }

    #[test]
    fn install_generic_failure_maps_to_failed_with_trimmed_stderr() {
        assert_eq!(
            classify_install_result(false, Some(1), "  no applicable package found  "),
            InstallOutcome::Failed("no applicable package found".to_string())
        );
    }

    #[test]
    fn install_success_maps_to_installed() {
        assert_eq!(
            classify_install_result(true, Some(0), ""),
            InstallOutcome::Installed
        );
    }

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

    #[test]
    fn fatal_on_request_validation_errors() {
        assert!(is_fatal_connect_error("The request failed: 400 Bad Request"));
        assert!(is_fatal_connect_error(
            "the tunnel port protocol cannot be changed"
        ));
        assert!(is_fatal_connect_error("error: invalid arguments"));
    }

    #[test]
    fn fatal_on_deleted_or_missing_tunnel() {
        // A deleted/expired tunnel surfaces while minting the host token; retrying
        // can never succeed, so it must stop instead of looping on `Authorizing`.
        assert!(is_fatal_connect_error("Tunnel not found in brs: fancy-ocean"));
        assert!(is_fatal_connect_error("The request was rejected: 404 Not Found"));
    }

    #[test]
    fn not_fatal_on_transient_connect_errors() {
        assert!(!is_fatal_connect_error("connection timed out"));
        assert!(!is_fatal_connect_error("503 Service Unavailable"));
        assert!(!is_fatal_connect_error("relay disconnected"));
    }

    #[test]
    fn missing_tunnel_detects_deleted_or_expired() {
        // Drives the auto-host prune: only a genuinely-gone tunnel, not every
        // fatal error (a 400 protocol mismatch must keep the group).
        assert!(is_missing_tunnel_error("Tunnel not found in brs: fancy-ocean"));
        assert!(is_missing_tunnel_error("The request was rejected: 404 Not Found"));
        assert!(!is_missing_tunnel_error("400 Bad Request"));
        assert!(!is_missing_tunnel_error("the tunnel port protocol cannot be changed"));
    }
}
