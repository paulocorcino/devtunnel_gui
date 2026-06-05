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

fn run_json<T: DeserializeOwned>(args: &[&str], loc: &Locale) -> Result<T> {
    let joined = args.join(" ");
    let output = Command::new(bin())
        .args(args)
        .output()
        .with_context(|| {
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

    // The CLI sometimes prints blank lines before the JSON; serde ignores leading whitespace.
    serde_json::from_slice(&output.stdout).with_context(|| {
        let mut a = FluentArgs::new();
        a.set("args", joined.clone());
        loc.t_args("err-cli-invalid-json", &a)
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
