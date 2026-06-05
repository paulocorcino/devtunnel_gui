//! SDK-backed host engine (issue #4), compiled only with `--features hosting`.
//!
//! A dedicated OS thread owns a single-threaded `tokio` runtime plus an mpsc
//! command receiver. Each [`HostCommand::Host`] starts a long-running task that:
//!   1. mints a `host` token (relay connect) and a `manage:ports` token (so the
//!      SDK's `add_port` → `create_tunnel_port` is authorized) via
//!      [`crate::devtunnel::mint_token`];
//!   2. builds a `TunnelManagementClient` + `TunnelLocator::ID` (cluster/id split
//!      by [`crate::devtunnel::split_locator`]);
//!   3. `RelayTunnelHost::connect(host_token)` then `add_port` for every port of
//!      the group (mirrors the proven sequence in `src/bin/host_spike.rs`);
//!   4. keeps the connection alive: on relay drop it reconnects with backoff, and
//!      a ~20h timer re-mints the tokens and reconnects before the ~24h expiry.
//!
//! [`HostCommand::Stop`] aborts the group's task (dropping the relay handle) and
//! emits [`HostState::Stopped`]. Every transition is reported via
//! [`HostEvent::State`].
//!
//! `Locale` is not `Send`, so it is constructed inside the engine thread from the
//! detected system locale and only used there.

use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::time::Duration;

use tokio::sync::mpsc as tokio_mpsc;
use tunnels::connections::RelayTunnelHost;
use tunnels::contracts::TunnelPort;
use tunnels::management::{new_tunnel_management, Authorization, TunnelLocator};

use super::{HostCommand, HostEvent, HostState};
use crate::devtunnel;
use crate::locale::{system_locale, Locale};

/// User-Agent reported to the tunnel management service.
const USER_AGENT: &str = "devtunnel-gui/0.1";
/// Re-mint the host/manage tokens before their ~24h expiry. 20h leaves headroom.
const REMINT_AFTER: Duration = Duration::from_secs(20 * 60 * 60);
/// Base backoff after a relay drop; doubles up to [`RECONNECT_BACKOFF_MAX`].
const RECONNECT_BACKOFF_START: Duration = Duration::from_secs(2);
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Starts the engine thread and returns its command channel. The caller wraps the
/// returned [`Sender`] in a [`super::TunnelHost`].
pub fn start(events: Sender<HostEvent>) -> std::sync::mpsc::Sender<HostCommand> {
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<HostCommand>();

    std::thread::Builder::new()
        .name("devtunnel-host".to_string())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    log::error!("host engine: failed to build tokio runtime: {e}");
                    return;
                }
            };
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, run(cmd_rx, events));
        })
        .expect("spawning the host engine thread should not fail");

    cmd_tx
}

/// Engine command loop. Owns a map of active host tasks keyed by Real Tunnel ID.
async fn run(cmd_rx: std::sync::mpsc::Receiver<HostCommand>, events: Sender<HostEvent>) {
    // The blocking std receiver is drained on a blocking task and forwarded onto a
    // tokio channel so the loop can `await` without parking the runtime thread.
    let (tok_tx, mut tok_rx) = tokio_mpsc::unbounded_channel::<HostCommand>();
    std::thread::spawn(move || {
        while let Ok(cmd) = cmd_rx.recv() {
            if tok_tx.send(cmd).is_err() {
                break;
            }
        }
    });

    let loc = Locale::load(&system_locale());
    let mut tasks: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();

    while let Some(cmd) = tok_rx.recv().await {
        match cmd {
            HostCommand::Host { tunnel_id } => {
                if tasks.get(&tunnel_id).is_some_and(|t| !t.is_finished()) {
                    log::debug!("host engine: already hosting {tunnel_id}, ignoring Host");
                    continue;
                }
                let ports = match collect_ports(&tunnel_id, &loc) {
                    Ok(ports) => ports,
                    Err(e) => {
                        emit(&events, &tunnel_id, HostState::Error(e.to_string()));
                        continue;
                    }
                };
                let events = events.clone();
                let id = tunnel_id.clone();
                let handle = tokio::task::spawn_local(async move {
                    host_group(id, ports, events).await;
                });
                tasks.insert(tunnel_id, handle);
            }
            HostCommand::Stop { tunnel_id } => {
                if let Some(handle) = tasks.remove(&tunnel_id) {
                    handle.abort();
                }
                emit(&events, &tunnel_id, HostState::Stopped);
            }
        }
    }
}

/// Fetches the port numbers defined for `tunnel_id` via the management CLI.
fn collect_ports(tunnel_id: &str, loc: &Locale) -> anyhow::Result<Vec<u16>> {
    let rows = devtunnel::fetch_rows(loc)?;
    let ports: Vec<u16> = rows
        .into_iter()
        .filter(|r| r.tunnel_id == tunnel_id && r.port > 0)
        .filter_map(|r| u16::try_from(r.port).ok())
        .collect();
    Ok(ports)
}

/// Long-running host task for one group: connect → add ports → keep alive, with
/// reconnect-on-drop and periodic token re-mint. Returns when aborted (Stop) or
/// on an unrecoverable error.
async fn host_group(tunnel_id: String, ports: Vec<u16>, events: Sender<HostEvent>) {
    let mut first_attempt = true;
    let mut backoff = RECONNECT_BACKOFF_START;

    loop {
        emit(
            &events,
            &tunnel_id,
            if first_attempt {
                HostState::Connecting
            } else {
                HostState::Reconnecting
            },
        );

        match connect_once(&tunnel_id, &ports).await {
            Ok(handle) => {
                backoff = RECONNECT_BACKOFF_START;
                first_attempt = false;
                emit(&events, &tunnel_id, HostState::Hosting);

                // Keep alive until the relay drops or the re-mint timer fires.
                tokio::select! {
                    r = handle => {
                        log::warn!("host engine: {tunnel_id} relay disconnected: {r:?}");
                        // Fall through to reconnect.
                    }
                    _ = tokio::time::sleep(REMINT_AFTER) => {
                        log::info!("host engine: {tunnel_id} re-minting tokens before expiry");
                        // Dropping `handle` here closes the current relay session;
                        // the loop reconnects with freshly minted tokens.
                    }
                }
            }
            Err(e) => {
                log::warn!("host engine: {tunnel_id} connect failed: {e}");
                first_attempt = false;
            }
        }

        // Backoff before the next (re)connect attempt.
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(RECONNECT_BACKOFF_MAX);
    }
}

/// One connect attempt: mint fresh tokens, build the client, connect, add ports.
/// `Locale` is rebuilt here because it is not `Send` across the `await` points of
/// the host task.
async fn connect_once(
    tunnel_id: &str,
    ports: &[u16],
) -> anyhow::Result<tunnels::connections::RelayHandle> {
    let loc = Locale::load(&system_locale());

    log::debug!("connect_once[{tunnel_id}]: minting host token");
    let host_token = devtunnel::mint_token(tunnel_id, "host", &loc)?;
    log::debug!("connect_once[{tunnel_id}]: minting manage:ports token");
    let manage_token = devtunnel::mint_token(tunnel_id, "manage:ports", &loc)?;

    let (cluster, id) = devtunnel::split_locator(tunnel_id).ok_or_else(|| {
        anyhow::anyhow!("tunnel id has no cluster suffix (expected 'id.cluster'): {tunnel_id}")
    })?;
    log::debug!("connect_once[{tunnel_id}]: locator cluster={cluster} id={id} ports={ports:?}");

    let mut builder = new_tunnel_management(USER_AGENT);
    builder.authorization(Authorization::Tunnel(manage_token));
    let mgmt = builder.into();
    let locator = TunnelLocator::ID { cluster, id };

    let mut host = RelayTunnelHost::new(locator, mgmt);
    log::debug!("connect_once[{tunnel_id}]: connecting to relay");
    let handle = host.connect(&host_token).await?;
    log::info!("connect_once[{tunnel_id}]: relay connected");

    for &port in ports {
        let tunnel_port = TunnelPort {
            port_number: port,
            protocol: Some("http".to_string()),
            ..Default::default()
        };
        // `add_port` treats an already-existing port (409) as success.
        log::debug!("connect_once[{tunnel_id}]: add_port {port}");
        host.add_port(&tunnel_port).await?;
        log::info!("connect_once[{tunnel_id}]: port {port} forwarded");
    }

    Ok(handle)
}

/// Sends a state transition to the UI, ignoring a closed channel (UI gone).
fn emit(events: &Sender<HostEvent>, tunnel_id: &str, state: HostState) {
    let _ = events.send(HostEvent::State {
        tunnel_id: tunnel_id.to_string(),
        state,
    });
}
