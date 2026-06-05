//! Camada de subprocesso do CLI `devtunnel` (sem PowerShell).
//! Invoca o binário direto via `std::process::Command` e desserializa a saída `-j`.
//! Roda sempre fora da thread de UI.

use crate::model::{ShowResult, TunnelList};
use anyhow::{anyhow, Context, Result};
use serde::de::DeserializeOwned;
use std::process::Command;

/// Uma porta achatada com sua URL, pronta para a UI.
pub struct Row {
    pub group: String,
    pub port: i32,
    pub protocol: String,
    pub url: String,
    pub expiration: String,
}

/// Resolve o binário. Permite override por `DEVTUNNEL_BIN`; senão confia no PATH.
fn bin() -> String {
    std::env::var("DEVTUNNEL_BIN").unwrap_or_else(|_| "devtunnel".to_string())
}

fn run_json<T: DeserializeOwned>(args: &[&str]) -> Result<T> {
    let output = Command::new(bin())
        .args(args)
        .output()
        .with_context(|| {
            format!(
                "falha ao executar `devtunnel {}` — o CLI está no PATH? \
                 (defina DEVTUNNEL_BIN se necessário)",
                args.join(" ")
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "`devtunnel {}` retornou erro: {}",
            args.join(" "),
            stderr.trim()
        ));
    }

    // O CLI às vezes imprime linhas em branco antes do JSON; serde ignora whitespace.
    serde_json::from_slice(&output.stdout)
        .with_context(|| format!("JSON inválido de `devtunnel {}`", args.join(" ")))
}

/// Enumera túneis (`list -j`) e, para cada um, busca portas + URLs (`show -j`).
pub fn fetch_rows() -> Result<Vec<Row>> {
    let list: TunnelList = run_json(&["list", "-j"])?;

    let mut rows = Vec::new();
    for t in list.tunnels {
        match run_json::<ShowResult>(&["show", &t.tunnel_id, "-j"]) {
            Ok(show) => {
                let exp = show.tunnel.tunnel_expiration;
                if show.tunnel.ports.is_empty() {
                    rows.push(Row {
                        group: t.tunnel_id.clone(),
                        port: 0,
                        protocol: String::new(),
                        url: String::new(),
                        expiration: exp,
                    });
                } else {
                    for p in show.tunnel.ports {
                        rows.push(Row {
                            group: t.tunnel_id.clone(),
                            port: p.port_number,
                            protocol: p.protocol,
                            url: p.port_uri.unwrap_or_default(),
                            expiration: exp.clone(),
                        });
                    }
                }
            }
            // Se o `show` falhar para um túnel, ainda mostramos o grupo com o que temos.
            Err(_) => rows.push(Row {
                group: t.tunnel_id.clone(),
                port: 0,
                protocol: String::new(),
                url: String::new(),
                expiration: t.tunnel_expiration,
            }),
        }
    }

    Ok(rows)
}
