//! Spike #2 (HITL): validates whether the `tunnels` SDK can host a tunnel in-process.
//!
//! Flow:
//!   1. Mint a host token via `devtunnel token <id> --scopes host -j` (subprocess).
//!   2. Start a minimal local HTTP server on the given port (something to forward).
//!   3. Build a TunnelManagementClient (anonymous auth) + RelayTunnelHost.
//!   4. connect(host_token) + add_port(port) → hosting in-process.
//!   5. Keep alive for ~120s for external validation (curl the Public URL).
//!
//! Usage:
//!   cargo run --features spike --bin host_spike -- <tunnel-id.cluster> <port>
//!   (defaults: paulo-desktop-diad0dn-3000.brs 3000)
//!
//! The port must already exist on the tunnel (created via CLI): add_port treats 409
//! as OK, so no management token is needed here — only the host token.

use std::process::Command;
use std::time::Duration;

use tunnels::connections::RelayTunnelHost;
use tunnels::contracts::TunnelPort;
use tunnels::management::{
    new_tunnel_management, Authorization, TunnelLocator, TunnelManagementClient,
};

const DEVTUNNEL: &str = "devtunnel";

fn devtunnel_bin() -> String {
    std::env::var("DEVTUNNEL_BIN").unwrap_or_else(|_| DEVTUNNEL.to_string())
}

/// `devtunnel token <id> --scopes <scope> -j` → token string.
/// Mint one scope at a time: repeating `--scopes` on the CLI corrupts the first value
/// (becomes "shost"). Two tokens are needed: `host` (connect to relay) and
/// `manage:ports` (SDK calls create_tunnel_port in add_port → 401 without auth).
fn mint_token(full_id: &str, scope: &str) -> anyhow::Result<String> {
    let out = Command::new(devtunnel_bin())
        .args(["token", full_id, "--scopes", scope, "-j"])
        .output()?;
    if !out.status.success() {
        anyhow::bail!(
            "devtunnel token ({scope}) failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    v.get("token")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("'token' field missing from devtunnel token output"))
}

/// Fetches the real Public URL (portUri) for the port via `devtunnel show <id> -j`.
fn fetch_port_uri(full_id: &str, port: u16) -> Option<String> {
    let out = Command::new(devtunnel_bin())
        .args(["show", full_id, "-j"])
        .output()
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let ports = v.get("tunnel")?.get("ports")?.as_array()?;
    for p in ports {
        if p.get("portNumber").and_then(|n| n.as_u64()) == Some(port as u64) {
            return p
                .get("portUri")
                .and_then(|u| u.as_str())
                .map(|s| s.to_string());
        }
    }
    None
}

/// Minimal local HTTP server: responds 200 with a known marker.
async fn run_local_server(port: u16) -> anyhow::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    log::info!("local test server listening on 127.0.0.1:{port}");
    loop {
        let (mut sock, _) = listener.accept().await?;
        tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            let _ = sock.read(&mut buf).await;
            let body = "DEVTUNNEL_SPIKE_OK\n";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.flush().await;
        });
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let mut args = std::env::args().skip(1);
    let full_id = args
        .next()
        .unwrap_or_else(|| "paulo-desktop-diad0dn-3000.brs".to_string());
    let port: u16 = args.next().and_then(|s| s.parse().ok()).unwrap_or(3000);

    // Split id and cluster at the last dot (e.g. "...-3000.brs" → id, "brs").
    let (id, cluster) = full_id
        .rsplit_once('.')
        .map(|(i, c)| (i.to_string(), c.to_string()))
        .ok_or_else(|| {
            anyhow::anyhow!("tunnel id has no cluster (expected 'id.cluster'): {full_id}")
        })?;

    println!("== SDK hosting spike ==");
    println!("tunnel id : {id}");
    println!("cluster   : {cluster}");
    println!("port      : {port}");

    // 1) tokens (one scope at a time)
    let host_token = mint_token(&full_id, "host")?;
    let manage_token = mint_token(&full_id, "manage:ports")?;
    println!(
        "tokens    : host={} chars, manage:ports={} chars",
        host_token.len(),
        manage_token.len()
    );

    // 2) local server
    tokio::spawn(async move {
        if let Err(e) = run_local_server(port).await {
            log::error!("local server failed: {e}");
        }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;

    // 3) management client authorized with the manage:ports token so that
    //    add_port → create_tunnel_port does not return 401.
    let mut builder = new_tunnel_management("devtunnel-gui-spike/0.1");
    builder.authorization(Authorization::Tunnel(manage_token));
    let mgmt: TunnelManagementClient = builder.into();
    let locator = TunnelLocator::ID { cluster, id };

    // 4) host
    let mut host = RelayTunnelHost::new(locator, mgmt);
    println!("connecting to relay…");
    let handle = host.connect(&host_token).await?;
    println!("relay connected ✓");

    let tunnel_port = TunnelPort {
        port_number: port,
        protocol: Some("http".to_string()),
        ..Default::default()
    };
    host.add_port(&tunnel_port).await?;
    println!("port {port} forwarded ✓");

    if let Some(uri) = fetch_port_uri(&full_id, port) {
        println!("\nPublic URL: {uri}");
        println!("Validate:    curl -s {uri}");
        println!("Expected:    DEVTUNNEL_SPIKE_OK\n");
    }

    println!("hosting for 120s (Ctrl-C to exit early)…");

    tokio::select! {
        r = handle => {
            println!("tunnel disconnected: {r:?}");
        }
        _ = tokio::time::sleep(Duration::from_secs(120)) => {
            println!("spike time elapsed.");
        }
    }

    Ok(())
}
