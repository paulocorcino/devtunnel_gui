//! Headless host runner — a diagnostic/test entrypoint (no GUI, no tray) used by
//! the blackbox E2E resilience harness in `tests/e2e/`.
//!
//! It drives the **production** host engine (`host::spawn` →
//! `engine::host_group` → the keep-alive driver), so the harness exercises the
//! real connect / keep-alive / reconnect path rather than a stand-in. It is
//! activated when `DEVTUNNEL_HEADLESS_HOST=<tunnel-id>[,<tunnel-id>…]` is set;
//! `main` returns through here before building any UI.
//!
//! Observability: every [`host::HostEvent`] is written as one JSON line on
//! stdout (logs stay on stderr via the capturing logger), so an external process
//! can observe state transitions deterministically. Control: it reads simple
//! line commands on stdin — `host <id>` (re-host), `stop <id>`, `stop` (all
//! groups), `drop <id>` (force a relay drop + reconnect without tearing the
//! group down — exercises the reconnect / token-reuse path of issue #47
//! deterministically, no firewall/admin needed), and `quit` (stop all and exit).
//! EOF on stdin is treated as `quit`.
//!
//! Only the `--features hosting` build has a real engine; the default build's
//! `NoopHost` makes this a no-op, which keeps the module compiling everywhere.

use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

use crate::host::{self, HostCommand, HostEvent, HostState};

/// A control command parsed from stdin.
enum Ctl {
    /// (Re)start hosting one group by Real Tunnel ID (used to re-host after a
    /// `stop`, exercising a clean teardown → reconnect cycle).
    Host(String),
    /// Stop one group by Real Tunnel ID.
    Stop(String),
    /// Force one group's relay to drop and reconnect without tearing it down
    /// (exercises the real reconnect / token-reuse path; issue #47).
    Drop(String),
    /// Stop every hosted group.
    StopAll,
    /// Stop everything and exit.
    Quit,
}

/// Runs the headless host loop for the comma-separated `ids_csv`. Returns once a
/// `quit` command (or stdin EOF) is received and the engine has been asked to
/// stop every group.
pub fn run(ids_csv: &str) -> anyhow::Result<()> {
    let ids: Vec<String> = ids_csv
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect();
    if ids.is_empty() {
        anyhow::bail!("DEVTUNNEL_HEADLESS_HOST is set but lists no tunnel ids");
    }

    let started = Instant::now();
    let (evt_tx, evt_rx) = std::sync::mpsc::channel::<HostEvent>();
    let host = host::spawn(evt_tx);

    for id in &ids {
        host.send(HostCommand::Host {
            tunnel_id: id.clone(),
        });
    }
    emit_line(&serde_json::json!({
        "elapsed_ms": started.elapsed().as_millis() as u64,
        "event": "started",
        "tunnel_ids": ids,
    }));

    // Stdin command reader → control channel. A dedicated thread keeps the main
    // thread free to drain host events without blocking on a stdin read.
    let (ctl_tx, ctl_rx) = std::sync::mpsc::channel::<Ctl>();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            let line = line.trim();
            let cmd = if line == "quit" || line == "exit" {
                Ctl::Quit
            } else if line == "stop" {
                Ctl::StopAll
            } else if let Some(rest) = line.strip_prefix("stop ") {
                Ctl::Stop(rest.trim().to_owned())
            } else if let Some(rest) = line.strip_prefix("drop ") {
                Ctl::Drop(rest.trim().to_owned())
            } else if let Some(rest) = line.strip_prefix("host ") {
                Ctl::Host(rest.trim().to_owned())
            } else {
                continue;
            };
            if ctl_tx.send(cmd).is_err() {
                return;
            }
        }
        // EOF on stdin → ask the main loop to quit.
        let _ = ctl_tx.send(Ctl::Quit);
    });

    // Main loop: interleave host events (printed as JSON) with control commands.
    // Poll the control channel with a short timeout so host events never starve.
    loop {
        loop {
            match evt_rx.try_recv() {
                Ok(evt) => emit_line(&event_json(started, &evt)),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                // The engine thread is gone; nothing more will arrive.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return Ok(()),
            }
        }
        match ctl_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ctl::Host(id)) => host.send(HostCommand::Host { tunnel_id: id }),
            Ok(Ctl::Stop(id)) => host.send(HostCommand::Stop { tunnel_id: id }),
            Ok(Ctl::Drop(id)) => host.send(HostCommand::DropRelay { tunnel_id: id }),
            Ok(Ctl::StopAll) => stop_all(host.as_ref(), &ids),
            Ok(Ctl::Quit) => {
                stop_all(host.as_ref(), &ids);
                // Give the engine a moment to emit the trailing `Stopped` events
                // before exiting, so the harness sees a clean teardown.
                std::thread::sleep(Duration::from_millis(300));
                while let Ok(evt) = evt_rx.try_recv() {
                    emit_line(&event_json(started, &evt));
                }
                return Ok(());
            }
            // No control input this tick: loop back and drain events again.
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            // The reader thread exited without a final Quit (should not happen);
            // keep draining events until the engine disconnects.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {}
        }
    }
}

/// Sends `Stop` for every group id.
fn stop_all(host: &dyn host::TunnelHost, ids: &[String]) {
    for id in ids {
        host.send(HostCommand::Stop {
            tunnel_id: id.clone(),
        });
    }
}

/// Renders one [`HostEvent`] as the JSON line emitted on stdout.
fn event_json(started: Instant, evt: &HostEvent) -> serde_json::Value {
    let elapsed_ms = started.elapsed().as_millis() as u64;
    match evt {
        HostEvent::State { tunnel_id, state } => {
            let (name, message) = match state {
                HostState::Idle => ("Idle", None),
                HostState::Connecting => ("Connecting", None),
                HostState::Hosting => ("Hosting", None),
                HostState::Reconnecting => ("Reconnecting", None),
                HostState::Stopped => ("Stopped", None),
                HostState::Error(m) => ("Error", Some(m.clone())),
            };
            serde_json::json!({
                "elapsed_ms": elapsed_ms,
                "event": "state",
                "tunnel_id": tunnel_id,
                "state": name,
                "message": message,
            })
        }
        HostEvent::Progress { tunnel_id, phase } => {
            // Additive to the `state` stream (issue #45): the coarse Connecting /
            // Hosting transitions still fire, so a harness keyed on those is
            // unaffected; this just exposes the sub-phase for finer diagnostics.
            let phase = match phase {
                host::ConnectPhase::Authorizing => "authorizing",
                host::ConnectPhase::ConnectingRelay => "connecting_relay",
                host::ConnectPhase::ForwardingPorts => "forwarding_ports",
            };
            serde_json::json!({
                "elapsed_ms": elapsed_ms,
                "event": "progress",
                "tunnel_id": tunnel_id,
                "phase": phase,
            })
        }
        HostEvent::ReloginRequired { tunnel_id } => serde_json::json!({
            "elapsed_ms": elapsed_ms,
            "event": "relogin_required",
            "tunnel_id": tunnel_id,
        }),
    }
}

/// Writes one JSON value as a line on stdout and flushes immediately so the
/// harness observes events in real time.
fn emit_line(v: &serde_json::Value) {
    println!("{v}");
    let _ = std::io::stdout().flush();
}
