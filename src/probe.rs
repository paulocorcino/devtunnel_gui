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
// Provisional relay-error body markers.
// These string fragments are expected in the HTML body of the devtunnels relay
// error page when the upstream local service is unreachable (HTTP 502/503).
// The exact values will be confirmed empirically in Stage 5 (HITL) and updated
// in src/probe.rs without changing the classify() signature.
// ---------------------------------------------------------------------------
const RELAY_ERROR_MARKERS: &[&str] =
    &["devtunnels", "tunnel", "Bad Gateway", "Service Unavailable"];

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

/// Classify a probe result into a `ProbeState`.
///
/// * `status` — HTTP status code returned by `ureq`, or `None` on I/O / network error.
/// * `body`   — Response body text (may be empty or partial; used only for 502/503).
///
/// # Classification logic
/// - `None` (network/timeout error)         → `Down`
/// - 502 or 503 **and** body contains a relay-error marker → `ServiceDown`
/// - 502 or 503 with no marker in the body  → `Down` (conservative: unknown relay behaviour)
/// - 2xx or any other status code           → `Operational`
pub fn classify(status: Option<u16>, body: &str) -> ProbeState {
    match status {
        None => ProbeState::Down,
        Some(code) if code == 502 || code == 503 => {
            let body_lower = body.to_lowercase();
            let is_relay_error = RELAY_ERROR_MARKERS
                .iter()
                .any(|marker| body_lower.contains(&marker.to_lowercase()));
            if is_relay_error {
                ProbeState::ServiceDown
            } else {
                ProbeState::Down
            }
        }
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

                // Probe every target.
                for target in &targets {
                    let (status, body) = match agent.get(&target.url).call() {
                        Ok(resp) => {
                            let code = resp.status();
                            let text = resp.into_string().unwrap_or_default();
                            (Some(code), text)
                        }
                        Err(ureq::Error::Status(code, resp)) => {
                            let text = resp.into_string().unwrap_or_default();
                            (Some(code), text)
                        }
                        Err(_) => (None, String::new()),
                    };

                    let state = classify(status, &body);
                    let _ = events.send(ProbeEvent::Status {
                        tunnel_id: target.tunnel_id.clone(),
                        port: target.port,
                        state,
                    });
                }

                // Sleep for the configured interval, checking for commands every second
                // so the thread stays responsive to SetInterval / SetTargets updates.
                let steps = interval.as_secs().max(1);
                for _ in 0..steps {
                    thread::sleep(Duration::from_secs(1));
                    // Quick check: if the command channel closed, exit.
                    match cmd_rx.try_recv() {
                        Ok(ProbeCommand::SetTargets(new_targets)) => {
                            targets = new_targets;
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
    fn classify_operational_on_2xx() {
        assert_eq!(classify(Some(200), ""), ProbeState::Operational);
        assert_eq!(classify(Some(204), ""), ProbeState::Operational);
        assert_eq!(classify(Some(301), ""), ProbeState::Operational);
        assert_eq!(classify(Some(404), ""), ProbeState::Operational);
    }

    #[test]
    fn classify_service_down_on_relay_502() {
        let relay_body = "Error: devtunnels relay could not reach the upstream tunnel service";
        assert_eq!(classify(Some(502), relay_body), ProbeState::ServiceDown);
    }

    #[test]
    fn classify_service_down_on_relay_503() {
        let relay_body = "Service Unavailable: the tunnel endpoint is not responding";
        assert_eq!(classify(Some(503), relay_body), ProbeState::ServiceDown);
    }

    #[test]
    fn classify_down_on_network_error() {
        assert_eq!(classify(None, ""), ProbeState::Down);
    }

    #[test]
    fn classify_down_on_502_without_relay_marker() {
        // A generic 502 (not from the devtunnels relay) should fall back to Down.
        assert_eq!(
            classify(Some(502), "some generic proxy error"),
            ProbeState::Down
        );
    }
}
