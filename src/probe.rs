//! Health probe engine — classifies each hosted port's state into one of three
//! values using a **hybrid** signal (issue #16).
//!
//! The authoritative **Down** signal (the host relay connection dropped) is owned
//! by the SDK's `RelayHandle` in [`crate::host::engine`], so the probe never needs
//! a WAN round-trip just to learn a group went offline. What remains is
//! distinguishing **Operational** from **ServiceDown** for a group that *is*
//! connected, and that is done with two complementary signals:
//!
//! 1. **Primary — local TCP connect** to `127.0.0.1`/`::1` on the forwarded port
//!    (mirrors the SDK forwarder). Near-zero cost, millisecond latency, no WAN
//!    traffic. Answers "is the local upstream listening?" — run every cycle.
//! 2. **Fallback — HTTP GET on the Public URL**, run only on a slow cadence. A
//!    local TCP check cannot see relay/protocol-level failures (e.g. a port
//!    configured `https` while the local server speaks plain HTTP returns a relay
//!    **502** even though the socket is listening), so the HTTP probe still runs
//!    occasionally to catch those. Its last result is cached between refreshes.
//!
//! Entire module is gated behind `#[cfg(feature = "hosting")]` so the default
//! `cargo build` stays lightweight.

#![cfg(feature = "hosting")]

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// 3-state health classification for a single hosted port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeState {
    /// The local upstream is listening and (when last checked over HTTP) the relay
    /// routed to it successfully.
    Operational,
    /// The local upstream port is not listening, or the relay reached it but the
    /// upstream returned a gateway error (HTTP 502/503/504 — e.g. a protocol
    /// mismatch). The host connection itself is still up.
    ServiceDown,
    /// The endpoint is unreachable (network error, timeout, DNS failure, etc.).
    /// In practice this is driven by the host engine's `RelayHandle`, not the probe.
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

/// Combine the cheap per-cycle local-TCP signal with the occasional HTTP fallback
/// into the final [`ProbeState`].
///
/// * `tcp_listening` — did a local `TcpStream::connect` to `127.0.0.1`/`::1` on the
///   forwarded port succeed this cycle?
/// * `http` — the cached HTTP fallback result: `None` if it has not run yet for this
///   target, `Some(status)` otherwise (inner `None` = network error).
///
/// # Logic
/// - Local port not listening → `ServiceDown`: the upstream service is down. This is
///   the common, cheap-to-detect case and needs no WAN traffic.
/// - Local port listening, but the last HTTP fallback saw a relay gateway error
///   (502/503/504) → `ServiceDown`: the socket accepts connections yet the relay
///   cannot get a valid response (e.g. an `https` port in front of a plain-HTTP
///   server). A local TCP check alone cannot see this.
/// - Otherwise → `Operational`. An HTTP network error while the local port *is*
///   listening is treated as a transient WAN hiccup, not a service outage — host
///   liveness is owned by the `RelayHandle`, not the probe.
pub fn combine(tcp_listening: bool, http: Option<Option<u16>>) -> ProbeState {
    if !tcp_listening {
        return ProbeState::ServiceDown;
    }
    match http {
        Some(status) if classify(status) == ProbeState::ServiceDown => ProbeState::ServiceDown,
        _ => ProbeState::Operational,
    }
}

/// Cheap local-upstream liveness check: does a TCP connection to the forwarded
/// `port` on loopback succeed within `timeout`? Tries `127.0.0.1` then `::1`,
/// mirroring how the SDK forwarder races both loopback families.
fn local_tcp_listening(port: u16, timeout: Duration) -> bool {
    let v4 = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    if TcpStream::connect_timeout(&v4, timeout).is_ok() {
        return true;
    }
    let v6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port);
    TcpStream::connect_timeout(&v6, timeout).is_ok()
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
            // Steady-state interval when all targets are healthy (overridable via SetInterval).
            let mut interval = Duration::from_secs(60);
            // Fast interval used while any target is not yet Operational, so a
            // recovering / warming-up service is reflected within a few seconds.
            // Each fast cycle does only the cheap local-TCP check (no WAN traffic).
            const FAST_INTERVAL: Duration = Duration::from_secs(3);
            // Timeout for the cheap loopback connect. Loopback either accepts almost
            // instantly or refuses; this only bounds a silently-dropping socket.
            const LOCAL_TCP_TIMEOUT: Duration = Duration::from_millis(800);
            // Slow cadence for the HTTP fallback on the Public URL — the expensive
            // WAN round-trip that catches relay/protocol failures a local TCP check
            // cannot see. Its result is cached for the local-TCP cycles in between.
            // Deliberately slower than the steady-green `interval` (60 s) so that a
            // healthy group is mostly served by the near-zero local-TCP check and
            // only pays a WAN round-trip "occasionally" (issue #16); otherwise the
            // fallback would fire every green cycle and save no WAN traffic at all.
            const HTTP_FALLBACK_INTERVAL: Duration = Duration::from_secs(300);

            // Build a reusable ureq agent with a 5 s connect+read timeout.
            let agent = ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(5))
                .timeout_read(Duration::from_secs(5))
                .build();

            // Cached HTTP fallback result, parallel to `targets` by index. `None`
            // means the fallback has not run for that target yet (treated as
            // Operational while the local port is listening); `Some(status)` is the
            // last observed HTTP status (inner `None` = network error). Rebuilt on
            // every SetTargets so a fresh set starts with no stale relay verdicts.
            let mut http_cache: Vec<Option<Option<u16>>> = Vec::new();
            // When the HTTP fallback last ran; `None` forces a refresh next cycle.
            let mut last_http: Option<Instant> = None;

            loop {
                // Drain pending commands before probing.
                loop {
                    match cmd_rx.try_recv() {
                        Ok(ProbeCommand::SetTargets(new_targets)) => {
                            http_cache = vec![None; new_targets.len()];
                            last_http = None; // refresh the HTTP fallback for the new set
                            targets = new_targets;
                        }
                        Ok(ProbeCommand::SetInterval(d)) => {
                            interval = d;
                        }
                        Err(mpsc::TryRecvError::Empty) => break,
                        Err(mpsc::TryRecvError::Disconnected) => return,
                    }
                }

                // The HTTP fallback runs only on its own slow cadence; the cheap
                // local-TCP check runs every cycle.
                let http_due = last_http.is_none_or(|t| t.elapsed() >= HTTP_FALLBACK_INTERVAL);
                if http_due {
                    last_http = Some(Instant::now());
                }

                // Probe every target with the hybrid signal.
                let mut all_operational = true;
                for (i, target) in targets.iter().enumerate() {
                    // Primary signal: cheap local TCP connect to the forwarded port.
                    let tcp_listening = match u16::try_from(target.port) {
                        Ok(p) => local_tcp_listening(p, LOCAL_TCP_TIMEOUT),
                        Err(_) => false,
                    };

                    // Fallback signal: HTTP GET on the Public URL, refreshed only on
                    // the slow cadence. Only the status code matters, so the response
                    // body is not downloaded.
                    if http_due {
                        let status = match agent.get(&target.url).call() {
                            Ok(resp) => Some(resp.status()),
                            Err(ureq::Error::Status(code, _)) => Some(code),
                            Err(_) => None,
                        };
                        if let Some(slot) = http_cache.get_mut(i) {
                            *slot = Some(status);
                        }
                    }
                    let http = http_cache.get(i).copied().flatten();

                    let state = combine(tcp_listening, http);
                    if state != ProbeState::Operational {
                        all_operational = false;
                    }
                    let _ = events.send(ProbeEvent::Status {
                        tunnel_id: target.tunnel_id.clone(),
                        port: target.port,
                        state,
                    });
                }

                // Adaptive cadence: while every target is healthy, poll at the slow
                // configured `interval` (low resource use). While anything is not yet
                // Operational (a freshly-hosted group still warming up, or a service
                // that's down/recovering), poll at a fast interval so the badge flips
                // within a few seconds instead of waiting up to `interval`.
                let wait = if targets.is_empty() || all_operational {
                    interval
                } else {
                    FAST_INTERVAL
                };

                // Sleep `wait`, checking commands every second so the thread stays
                // responsive. A SetTargets update breaks the sleep early so new
                // targets are probed immediately.
                let steps = wait.as_secs().max(1);
                for _ in 0..steps {
                    thread::sleep(Duration::from_secs(1));
                    match cmd_rx.try_recv() {
                        Ok(ProbeCommand::SetTargets(new_targets)) => {
                            http_cache = vec![None; new_targets.len()];
                            last_http = None; // refresh the HTTP fallback for the new set
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

    #[test]
    fn combine_service_down_when_local_port_closed() {
        // The cheap local-TCP signal is authoritative for "service down": if the
        // loopback port is not listening, the HTTP fallback is irrelevant.
        assert_eq!(combine(false, None), ProbeState::ServiceDown);
        assert_eq!(combine(false, Some(Some(200))), ProbeState::ServiceDown);
        assert_eq!(combine(false, Some(None)), ProbeState::ServiceDown);
    }

    #[test]
    fn combine_operational_when_listening_and_no_http_yet() {
        // Local port up and the HTTP fallback has not run for this target yet:
        // assume Operational rather than flashing a false ServiceDown.
        assert_eq!(combine(true, None), ProbeState::Operational);
    }

    #[test]
    fn combine_operational_when_listening_and_http_ok() {
        assert_eq!(combine(true, Some(Some(200))), ProbeState::Operational);
        assert_eq!(combine(true, Some(Some(404))), ProbeState::Operational);
    }

    #[test]
    fn combine_service_down_on_relay_gateway_error() {
        // Listening locally but the relay returned a gateway error (e.g. an https
        // port in front of a plain-HTTP server) — only the HTTP fallback sees this.
        assert_eq!(combine(true, Some(Some(502))), ProbeState::ServiceDown);
        assert_eq!(combine(true, Some(Some(503))), ProbeState::ServiceDown);
        assert_eq!(combine(true, Some(Some(504))), ProbeState::ServiceDown);
    }

    #[test]
    fn combine_operational_on_transient_http_network_error() {
        // Local port is up but the WAN HTTP probe failed: treat as a transient
        // hiccup, not a service outage — host liveness is the RelayHandle's job.
        assert_eq!(combine(true, Some(None)), ProbeState::Operational);
    }
}
