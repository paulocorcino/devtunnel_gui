//! SDK-backed host engine (issue #4), compiled only with `--features hosting`.
//!
//! A lightweight command thread owns no async runtime of its own; it just
//! dispatches [`HostCommand`]s. Each [`HostCommand::Host`] spawns a **dedicated
//! OS thread** for that group, owning its own single-threaded `tokio` runtime +
//! `LocalSet`. Isolating every group on its own runtime stops one busy tunnel
//! from starving another's port forwards (issue #18) — the previous design ran
//! all groups' relay + `forward_port_to_tcp` tasks on one shared thread, which
//! stalled some forwards under concurrent multi-tunnel traffic.
//!
//! Each group thread runs a long-running task that:
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
//! [`HostCommand::Stop`] signals the group's cancellation [`Notify`], which ends
//! its `block_on` (dropping the runtime aborts the relay + forward tasks) and
//! emits [`HostState::Stopped`]. Every transition is reported via
//! [`HostEvent::State`].
//!
//! `Locale` is not `Send`, so it is constructed inside each thread from the
//! detected system locale and only used there.

use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use tokio::sync::Notify;
use tunnels::connections::RelayTunnelHost;
use tunnels::contracts::TunnelPort;
use tunnels::management::{new_tunnel_management, Authorization, TunnelLocator};

use super::{ConnectPhase, HostCommand, HostEvent, HostState};
use crate::devtunnel;
use crate::locale::{system_locale, Locale};

/// User-Agent reported to the tunnel management service.
const USER_AGENT: &str = "devtunnel-gui/0.1";

/// Starts the engine command thread and returns its command channel. The caller
/// wraps the returned [`Sender`] in a [`super::TunnelHost`].
pub fn start(events: Sender<HostEvent>) -> std::sync::mpsc::Sender<HostCommand> {
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<HostCommand>();

    std::thread::Builder::new()
        .name("devtunnel-host".to_string())
        .spawn(move || run(cmd_rx, events))
        .expect("spawning the host engine command thread should not fail");

    cmd_tx
}

/// Handle to a per-group worker thread: its join handle (used only to check
/// liveness on a repeat `Host`) and a cancellation [`Notify`] that, when
/// signalled, ends the group's `block_on` so its runtime drops.
struct GroupHandle {
    thread: std::thread::JoinHandle<()>,
    cancel: Arc<Notify>,
}

/// Engine command loop. Runs on its own OS thread with no async runtime of its
/// own — it only dispatches commands. Owns a map of per-group worker threads
/// keyed by Real Tunnel ID; each group is fully isolated on its own runtime.
fn run(cmd_rx: std::sync::mpsc::Receiver<HostCommand>, events: Sender<HostEvent>) {
    let loc = Locale::load(&system_locale());
    let mut groups: HashMap<String, GroupHandle> = HashMap::new();

    while let Ok(cmd) = cmd_rx.recv() {
        match cmd {
            HostCommand::Host { tunnel_id } => {
                if groups
                    .get(&tunnel_id)
                    .is_some_and(|g| !g.thread.is_finished())
                {
                    log::debug!("host engine: already hosting {tunnel_id}, ignoring Host");
                    continue;
                }
                let ports = match devtunnel::fetch_tunnel_ports(&tunnel_id, &loc) {
                    Ok(ports) => ports,
                    Err(e) => {
                        let msg = e.to_string();
                        // An expired/absent CLI sign-in is not a generic error:
                        // surface it so the UI can offer "Sign in".
                        if devtunnel::is_auth_error(&msg) {
                            let _ = events.send(HostEvent::ReloginRequired {
                                tunnel_id: tunnel_id.clone(),
                            });
                        }
                        emit(&events, &tunnel_id, HostState::Error(msg));
                        continue;
                    }
                };
                let handle = spawn_group(tunnel_id.clone(), ports, events.clone());
                groups.insert(tunnel_id, handle);
            }
            HostCommand::Stop { tunnel_id } => {
                if let Some(group) = groups.remove(&tunnel_id) {
                    // Wake the group's `select!` so `block_on` returns; dropping its
                    // runtime aborts the relay + forward tasks. The thread is left
                    // to wind down on its own (teardown is just dropping handles).
                    group.cancel.notify_one();
                }
                emit(&events, &tunnel_id, HostState::Stopped);
            }
        }
    }
}

/// Spawns a dedicated OS thread for one group, owning its own current-thread
/// tokio runtime + `LocalSet` (the SDK's relay/russh state is `!Send`, so each
/// host task must run via `block_on` on a `LocalSet`). The host task races a
/// cancellation [`Notify`]: a `Stop` signals it, `block_on` returns, and the
/// runtime drop tears the group down. Isolating each group on its own runtime is
/// the fix for multi-tunnel forward starvation (issue #18).
fn spawn_group(
    tunnel_id: String,
    ports: Vec<(u16, String)>,
    events: Sender<HostEvent>,
) -> GroupHandle {
    let cancel = Arc::new(Notify::new());
    let cancel_signal = cancel.clone();

    let thread = std::thread::Builder::new()
        .name(format!("devtunnel-host-{tunnel_id}"))
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    log::error!("host engine: failed to build runtime for {tunnel_id}: {e}");
                    emit(&events, &tunnel_id, HostState::Error(e.to_string()));
                    return;
                }
            };
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async {
                tokio::select! {
                    _ = host_group(tunnel_id, ports, events) => {}
                    _ = cancel_signal.notified() => {}
                }
            });
        })
        .expect("spawning a per-group host thread should not fail");

    GroupHandle { thread, cancel }
}

/// Long-running host task for one group: connect → add ports → keep alive, with
/// reconnect-on-drop and periodic token re-mint. Loops forever; the caller's
/// `select!` ends it when the group is cancelled (Stop). Returns early only on an
/// unrecoverable error (e.g. expired sign-in).
async fn host_group(tunnel_id: String, ports: Vec<(u16, String)>, events: Sender<HostEvent>) {
    use super::keepalive::{Action, ConnEvent, ConnFailure, KeepAliveState, Phase};

    let mut state = KeepAliveState::new();

    loop {
        emit(
            &events,
            &tunnel_id,
            match state.phase() {
                Phase::Initial => HostState::Connecting,
                Phase::Reconnect => HostState::Reconnecting,
            },
        );

        let action = match connect_once(&tunnel_id, &ports, &events).await {
            // INVARIANT: `_host` (the `RelayTunnelHost`) MUST stay bound across
            // the keep-alive `select!` below — it owns the `ports_tx`
            // watch::Sender that every client's `run_stream` task waits on. The
            // SDK's `run_stream` ignores the `Result` from `ports.changed()`, so
            // once that sender is dropped, `changed()` returns `Err` forever and
            // each task spins a CPU core (observed: ~2.5 cores pegged → freeze
            // under client churn). The state machine is pure and channel-free,
            // so the wait stays inline here: `_host` must not be moved into a
            // helper that drops it before the await. The only early `return` is
            // in the `Err` arm, where no live host is bound.
            Ok((_host, handle)) => {
                // Success resets the backoff and leaves the first-attempt phase.
                let _ = state.next(ConnEvent::Connected);
                emit(&events, &tunnel_id, HostState::Hosting);

                // Keep alive until the relay drops or the re-mint timer fires.
                let event = tokio::select! {
                    r = handle => {
                        log::warn!("host engine: {tunnel_id} relay disconnected: {r:?}");
                        ConnEvent::RelayDropped
                    }
                    _ = tokio::time::sleep(super::keepalive::REMINT_AFTER) => {
                        log::info!("host engine: {tunnel_id} re-minting tokens before expiry");
                        ConnEvent::RemintDue
                    }
                };
                // `_host` and the unfinished `handle` both drop here on the way
                // to reconnect, tearing down the relay session so old
                // `run_stream` tasks exit via their stream-closed arm.
                state.next(event)
            }
            Err(e) => {
                let msg = e.to_string();
                // Classify the raw error into the policy's failure kind. Auth is
                // checked first because it has a dedicated recovery path; a 400
                // from the management API (e.g. a port-protocol mismatch) is
                // otherwise non-recoverable and must not loop forever (issue #36).
                let failure = if devtunnel::is_auth_error(&msg) {
                    ConnFailure::Auth
                } else if devtunnel::is_fatal_connect_error(&msg) {
                    ConnFailure::Fatal
                } else {
                    ConnFailure::Transient
                };
                let action = state.next(ConnEvent::ConnectFailed(failure));
                match action {
                    // Sign-in expired: end the task and prompt re-auth (auto-resume
                    // re-hosts after a successful sign-in).
                    Action::Relogin => {
                        log::warn!("host engine: {tunnel_id} login expired: {msg}");
                        let _ = events.send(HostEvent::ReloginRequired {
                            tunnel_id: tunnel_id.clone(),
                        });
                        emit(&events, &tunnel_id, HostState::Error(msg));
                        return;
                    }
                    // Non-recoverable: surface the error and stop instead of
                    // retrying identical inputs in an endless backoff loop.
                    Action::Fail => {
                        log::warn!("host engine: {tunnel_id} non-recoverable connect error: {msg}");
                        emit(&events, &tunnel_id, HostState::Error(msg));
                        return;
                    }
                    _ => {
                        log::warn!("host engine: {tunnel_id} connect failed: {e}");
                        action
                    }
                }
            }
        };

        // Execute the policy's decision for the next (re)connect attempt.
        match action {
            Action::Sleep(d) => tokio::time::sleep(d).await,
            // `Await` only follows a `Connected` event, which the Ok arm
            // overwrites with the keep-alive outcome before reaching here;
            // `Relogin`/`Fail` return in the Err arm above. None are reachable.
            Action::Await | Action::Relogin | Action::Fail => {}
        }
    }
}

/// One connect attempt: mint fresh tokens, build the client, connect, add ports.
/// `Locale` is rebuilt here because it is not `Send` across the `await` points of
/// the host task.
///
/// Returns the live [`RelayTunnelHost`] **and** its [`RelayHandle`]. The caller
/// must keep the host bound for the lifetime of the connection: it owns the
/// `ports_tx` watch::Sender that the SDK's per-client `run_stream` tasks wait on,
/// and dropping it early makes those tasks busy-loop (see [`host_group`]).
async fn connect_once(
    tunnel_id: &str,
    ports: &[(u16, String)],
    events: &Sender<HostEvent>,
) -> anyhow::Result<(RelayTunnelHost, tunnels::connections::RelayHandle)> {
    // Surface each connect sub-phase so a multi-second wait shows progress
    // instead of one static "Connecting" label (issue #45).
    progress(events, tunnel_id, ConnectPhase::Authorizing);
    // Mint both tokens concurrently on blocking threads. `mint_token` is a
    // blocking subprocess + network round-trip; running the two sequentially on
    // this current-thread runtime both doubles the wait and — during a re-mint —
    // stalls the *still-live* relay + port-forward tasks that share this
    // executor, widening the very outage the re-mint is meant to avoid.
    // `spawn_blocking` moves each mint off the executor so the old connection
    // keeps forwarding while the new tokens mint, and `try_join!` overlaps the
    // two round-trips. `Locale` is `!Send`, so each closure builds its own from
    // the system locale (used only for error formatting).
    log::debug!("connect_once[{tunnel_id}]: minting host + manage:ports tokens");
    let host_task = {
        let id = tunnel_id.to_string();
        tokio::task::spawn_blocking(move || {
            devtunnel::mint_token(&id, "host", &Locale::load(&system_locale()))
        })
    };
    let manage_task = {
        let id = tunnel_id.to_string();
        tokio::task::spawn_blocking(move || {
            devtunnel::mint_token(&id, "manage:ports", &Locale::load(&system_locale()))
        })
    };
    let (host_res, manage_res) = tokio::try_join!(host_task, manage_task)
        .map_err(|e| anyhow::anyhow!("token mint task panicked: {e}"))?;
    let host_token = host_res?;
    let manage_token = manage_res?;

    let (cluster, id) = devtunnel::split_locator(tunnel_id).ok_or_else(|| {
        anyhow::anyhow!("tunnel id has no cluster suffix (expected 'id.cluster'): {tunnel_id}")
    })?;
    log::debug!("connect_once[{tunnel_id}]: locator cluster={cluster} id={id} ports={ports:?}");

    let mut builder = new_tunnel_management(USER_AGENT);
    builder.authorization(Authorization::Tunnel(manage_token));
    let mgmt = builder.into();
    let locator = TunnelLocator::ID { cluster, id };

    let mut host = RelayTunnelHost::new(locator, mgmt);
    progress(events, tunnel_id, ConnectPhase::ConnectingRelay);
    log::debug!("connect_once[{tunnel_id}]: connecting to relay");
    let handle = host.connect(&host_token).await?;
    log::info!("connect_once[{tunnel_id}]: relay connected");

    if !ports.is_empty() {
        progress(events, tunnel_id, ConnectPhase::ForwardingPorts);
    }
    for (port, protocol) in ports {
        // Forward each port under its configured protocol. The service rejects a
        // re-registration that changes the protocol, so an `https`/`auto` port
        // forwarded as `http` would 400 and block hosting (issue #36). Fall back
        // to `auto` only when the protocol is genuinely absent.
        let proto = if protocol.trim().is_empty() {
            "auto"
        } else {
            protocol.as_str()
        };
        let tunnel_port = TunnelPort {
            port_number: *port,
            protocol: Some(proto.to_string()),
            ..Default::default()
        };
        // `add_port` treats an already-existing port (409) as success.
        log::debug!("connect_once[{tunnel_id}]: add_port {port} ({proto})");
        host.add_port(&tunnel_port).await?;
        log::info!("connect_once[{tunnel_id}]: port {port} forwarded ({proto})");
    }

    Ok((host, handle))
}

/// Sends a state transition to the UI, ignoring a closed channel (UI gone).
fn emit(events: &Sender<HostEvent>, tunnel_id: &str, state: HostState) {
    let _ = events.send(HostEvent::State {
        tunnel_id: tunnel_id.to_string(),
        state,
    });
}

/// Sends a connect sub-phase to the UI, ignoring a closed channel (UI gone).
fn progress(events: &Sender<HostEvent>, tunnel_id: &str, phase: ConnectPhase) {
    let _ = events.send(HostEvent::Progress {
        tunnel_id: tunnel_id.to_string(),
        phase,
    });
}
