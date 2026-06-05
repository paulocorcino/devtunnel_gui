//! Health probe engine — periodically GETs each hosted port's Public URL and
//! classifies its state into one of three values.
//!
//! Entire module is gated behind `#[cfg(feature = "hosting")]` so the default
//! `cargo build` stays lightweight.

#![cfg(feature = "hosting")]

use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// 3-state health classification for a single hosted port URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeState {
    /// The relay is reachable and the local upstream service responded normally.
    Operational,
    /// The relay answered (HTTP 502 or 503) but the upstream is down.
    ServiceDown,
    /// The endpoint is unreachable (network error, timeout, DNS failure, etc.).
    Down,
}

/// Events emitted by the probe thread to the UI/wiring layer.
#[derive(Debug, Clone)]
pub enum ProbeEvent {
    /// Probe result for a single port.
    Status {
        tunnel_id: String,
        port: i32,
        state: ProbeState,
    },
}

/// Commands sent to the probe thread.
pub enum ProbeCommand {
    /// Replace the full set of targets to probe.
    SetTargets(Vec<ProbeTarget>),
    /// Override the probe interval (default: 60 s).
    SetInterval(Duration),
}

/// A single probe target describing one hosted port.
pub struct ProbeTarget {
    pub tunnel_id: String,
    pub port: i32,
    pub url: String,
}

// ---------------------------------------------------------------------------
// Pure classifier (unit-tested below)
// ---------------------------------------------------------------------------

/// Classify a probe result into a `ProbeState`, based purely on the HTTP status.
///
/// * `status` — HTTP status code returned by `ureq`, or `None` on I/O / network error.
///
/// # Classification logic
/// - `None` (network/timeout/DNS error) → `Down` — the Public URL is unreachable
///   (relay down, or the group is not hosted at all).
/// - `502` / `503` / `504` → `ServiceDown` — the devtunnels relay was reached but
///   could not get a valid response from the local upstream. Confirmed empirically
///   in Stage 5: a port configured `https` while the local server speaks plain HTTP
///   returns a relay **502** even though the local service is up. These gateway
///   codes are emitted by the relay, not the app, so the status alone is a reliable
///   signal — no body-string matching needed.
/// - Any other status (2xx/3xx/4xx) → `Operational` — the relay routed to the
///   upstream and it answered. A 401/404 here is the *local app's* own response,
///   i.e. the service is reachable. (Edge case: if the host connection silently
///   dropped, the relay can answer 404 itself; in practice a dropped host emits a
///   HostEvent that clears the probe state before this matters.)
pub fn classify(status: Option<u16>) -> ProbeState {
    match status {
        None => ProbeState::Down,
        Some(502) | Some(503) | Some(504) => ProbeState::ServiceDown,
        Some(_) => ProbeState::Operational,
    }
}

// ---------------------------------------------------------------------------
// Probe thread
// ---------------------------------------------------------------------------

/// Spawn the probe background thread.
///
/// Returns a `Sender<ProbeCommand>` for controlling the thread and a
/// `std::sync::mpsc::Receiver<ProbeEvent>` for receiving results.
pub fn spawn(events: Sender<ProbeEvent>) -> Sender<ProbeCommand> {
    let (cmd_tx, cmd_rx) = mpsc::channel::<ProbeCommand>();

    thread::Builder::new()
        .name("probe-engine".into())
        .spawn(move || {
            let mut targets: Vec<ProbeTarget> = Vec::new();
            let mut interval = Duration::from_secs(60);

            // Build a reusable ureq agent with a 5 s connect+read timeout.
            let agent = ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(5))
                .timeout_read(Duration::from_secs(5))
                .build();

            loop {
                // Drain pending commands before probing.
                loop {
                    match cmd_rx.try_recv() {
                        Ok(ProbeCommand::SetTargets(new_targets)) => {
                            targets = new_targets;
                        }
                        Ok(ProbeCommand::SetInterval(d)) => {
                            interval = d;
                        }
                        Err(mpsc::TryRecvError::Empty) => break,
                        Err(mpsc::TryRecvError::Disconnected) => return,
                    }
                }

                // Probe every target. Only the status code matters for the 3-state
                // classification, so the response body is not downloaded.
                for target in &targets {
                    let status = match agent.get(&target.url).call() {
                        Ok(resp) => Some(resp.status()),
                        Err(ureq::Error::Status(code, _)) => Some(code),
                        Err(_) => None,
                    };

                    let state = classify(status);
                    let _ = events.send(ProbeEvent::Status {
                        tunnel_id: target.tunnel_id.clone(),
                        port: target.port,
                        state,
                    });
                }

                // Sleep for the configured interval, checking for commands every second
                // so the thread stays responsive to SetInterval / SetTargets updates.
                // A SetTargets update breaks the sleep early so the new targets are
                // probed immediately (otherwise a freshly-hosted group would show no
                // health badge for up to `interval` seconds).
                let steps = interval.as_secs().max(1);
                for _ in 0..steps {
                    thread::sleep(Duration::from_secs(1));
                    match cmd_rx.try_recv() {
                        Ok(ProbeCommand::SetTargets(new_targets)) => {
                            targets = new_targets;
                            break; // re-probe immediately with the new targets
                        }
                        Ok(ProbeCommand::SetInterval(d)) => {
                            interval = d;
                        }
                        Err(mpsc::TryRecvError::Empty) => {}
                        Err(mpsc::TryRecvError::Disconnected) => return,
                    }
                }
            }
        })
        .expect("failed to spawn probe-engine thread");

    cmd_tx
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_operational_on_2xx_3xx() {
        assert_eq!(classify(Some(200)), ProbeState::Operational);
        assert_eq!(classify(Some(204)), ProbeState::Operational);
        assert_eq!(classify(Some(301)), ProbeState::Operational);
    }

    #[test]
    fn classify_operational_on_app_4xx() {
        // A 401/404 here is the local app's own response: the service is reachable
        // through the relay, so the tunnel is Operational.
        assert_eq!(classify(Some(401)), ProbeState::Operational);
        assert_eq!(classify(Some(404)), ProbeState::Operational);
    }

    #[test]
    fn classify_service_down_on_gateway_errors() {
        // Relay-emitted gateway errors mean the upstream local service is unreachable
        // (confirmed in Stage 5 via an https-port / http-server protocol mismatch).
        assert_eq!(classify(Some(502)), ProbeState::ServiceDown);
        assert_eq!(classify(Some(503)), ProbeState::ServiceDown);
        assert_eq!(classify(Some(504)), ProbeState::ServiceDown);
    }

    #[test]
    fn classify_down_on_network_error() {
        assert_eq!(classify(None), ProbeState::Down);
    }
}
