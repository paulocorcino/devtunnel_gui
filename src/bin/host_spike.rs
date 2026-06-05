//! Spike #2 (HITL): valida se o SDK `tunnels` hospeda um túnel em processo.
//!
//! Fluxo:
//!   1. Emite um host token via `devtunnel token <id> --scopes host -j` (subprocesso).
//!   2. Sobe um servidor HTTP local mínimo na porta (algo para encaminhar).
//!   3. Constrói o TunnelManagementClient (auth anônima) + RelayTunnelHost.
//!   4. connect(host_token) + add_port(porta) → hospeda em processo.
//!   5. Mantém vivo por ~120s para validação externa (curl na Public URL).
//!
//! Uso:
//!   cargo run --features spike --bin host_spike -- <tunnel-id.cluster> <porta>
//!   (defaults: paulo-desktop-diad0dn-3000.brs 3000)
//!
//! A porta já deve existir no túnel (criada via CLI): add_port trata 409 como OK,
//! então não precisamos de token de management aqui — só do host token.

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
/// Mintamos um escopo por vez: repetir `--scopes` no CLI corrompe o 1º valor
/// (vira "shost"). Precisamos de dois tokens: `host` (conectar ao relay) e
/// `manage:ports` (o SDK chama create_tunnel_port em add_port → 401 sem auth).
fn mint_token(full_id: &str, scope: &str) -> anyhow::Result<String> {
    let out = Command::new(devtunnel_bin())
        .args(["token", full_id, "--scopes", scope, "-j"])
        .output()?;
    if !out.status.success() {
        anyhow::bail!(
            "devtunnel token ({scope}) falhou: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    v.get("token")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("campo 'token' ausente na saída de devtunnel token"))
}

/// Busca a Public URL real (portUri) da porta via `devtunnel show <id> -j`.
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

/// Servidor HTTP local mínimo: responde 200 com um marcador conhecido.
async fn run_local_server(port: u16) -> anyhow::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    log::info!("servidor local de teste ouvindo em 127.0.0.1:{port}");
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
    let port: u16 = args
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000);

    // Separa id e cluster no último ponto (ex.: "...-3000.brs" → id, "brs").
    let (id, cluster) = full_id
        .rsplit_once('.')
        .map(|(i, c)| (i.to_string(), c.to_string()))
        .ok_or_else(|| anyhow::anyhow!("tunnel id sem cluster (esperado 'id.cluster'): {full_id}"))?;

    println!("== Spike de hospedagem via SDK ==");
    println!("tunnel id : {id}");
    println!("cluster   : {cluster}");
    println!("porta     : {port}");

    // 1) tokens (um escopo por vez)
    let host_token = mint_token(&full_id, "host")?;
    let manage_token = mint_token(&full_id, "manage:ports")?;
    println!(
        "tokens    : host={} chars, manage:ports={} chars",
        host_token.len(),
        manage_token.len()
    );

    // 2) servidor local
    tokio::spawn(async move {
        if let Err(e) = run_local_server(port).await {
            log::error!("servidor local falhou: {e}");
        }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;

    // 3) management client autorizado com o token manage:ports,
    //    para que add_port → create_tunnel_port não tome 401.
    let mut builder = new_tunnel_management("devtunnel-gui-spike/0.1");
    builder.authorization(Authorization::Tunnel(manage_token));
    let mgmt: TunnelManagementClient = builder.into();
    let locator = TunnelLocator::ID { cluster, id };

    // 4) hospeda
    let mut host = RelayTunnelHost::new(locator, mgmt);
    println!("conectando ao relay…");
    let handle = host.connect(&host_token).await?;
    println!("relay conectado ✓");

    let tunnel_port = TunnelPort {
        port_number: port,
        protocol: Some("http".to_string()),
        ..Default::default()
    };
    host.add_port(&tunnel_port).await?;
    println!("porta {port} encaminhada ✓");

    if let Some(uri) = fetch_port_uri(&full_id, port) {
        println!("\nPublic URL: {uri}");
        println!("Valide com:  curl -s {uri}");
        println!("Esperado:    DEVTUNNEL_SPIKE_OK\n");
    }

    println!("hospedando por 120s (Ctrl-C para sair antes)…");

    tokio::select! {
        r = handle => {
            println!("túnel desconectou: {r:?}");
        }
        _ = tokio::time::sleep(Duration::from_secs(120)) => {
            println!("tempo do spike encerrado.");
        }
    }

    Ok(())
}
