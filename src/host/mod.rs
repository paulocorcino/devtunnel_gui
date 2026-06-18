//! Host control surface for the integrated SDK hosting engine (issue #4).
//!
//! This module compiles two ways:
//! - with `--features hosting`: `spawn` returns a placeholder host (the real
//!   SDK-backed engine lands in Stage 2);
//! - without it: `spawn` returns a [`NoopHost`] that silently drops commands so
//!   the default `cargo build` stays light (no SDK / vendored OpenSSL).
//!
//! The public type surface (`HostState`, `HostCommand`, `HostEvent`,
//! `TunnelHost`) is shared by both variants and is consumed by the UI layer.

// The control surface is defined here but not wired into the UI until Stage 4;
// allow dead code so the skeleton compiles cleanly in the meantime.
#![allow(dead_code)]

use std::sync::mpsc::Sender;

// The pure keep-alive state machine (issue #35) lives in `keepalive.rs`. It has
// zero SDK deps, so it is declared unconditionally — its tests run under a plain
// `cargo test` without the vendored-OpenSSL toolchain. The `#![allow(dead_code)]`
// above keeps the items it exposes but the default build never calls from
// warning.
mod keepalive;

// The real SDK-backed engine (connect/keep-alive/stop) lives in `engine.rs` and
// is compiled only with `--features hosting`.
#[cfg(feature = "hosting")]
mod engine;

/// Lifecycle state of a hosted group, reported back to the UI via [`HostEvent`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostState {
    /// Not hosting; no connection attempt in progress.
    Idle,
    /// Establishing the relay connection.
    Connecting,
    /// Connected and serving the group's ports.
    Hosting,
    /// Connection dropped; attempting to re-establish it.
    Reconnecting,
    /// Hosting was explicitly stopped (definition persists in the service).
    Stopped,
    /// A non-recoverable error occurred; carries a human-readable message.
    Error(String),
}

/// A sub-phase of an in-progress connect, reported via [`HostEvent::Progress`]
/// so a multi-second connect shows what it is doing instead of a single static
/// "Connecting" label (issue #45). Purely informational: it does not change the
/// coarse [`HostState`] lifecycle, so consumers that only track Connecting /
/// Hosting / Reconnecting can ignore it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectPhase {
    /// Minting the `host` + `manage:ports` tokens.
    Authorizing,
    /// Establishing the relay connection (TLS/SSH handshake).
    ConnectingRelay,
    /// Registering the group's ports on the relay.
    ForwardingPorts,
}

/// A command sent to the host engine.
#[derive(Debug, Clone)]
pub enum HostCommand {
    /// Start hosting the given group (Real Tunnel ID).
    Host { tunnel_id: String },
    /// Stop hosting the given group; its definition is left intact.
    Stop { tunnel_id: String },
}

/// An event emitted by the host engine for the UI to consume.
#[derive(Debug, Clone)]
pub enum HostEvent {
    /// A group's hosting state changed.
    State { tunnel_id: String, state: HostState },
    /// A group's connect advanced to a new sub-phase (issue #45). Additive to
    /// [`HostEvent::State`]: the coarse Connecting/Hosting transitions still
    /// fire, so a consumer can ignore this without missing any lifecycle change.
    Progress {
        tunnel_id: String,
        phase: ConnectPhase,
    },
    /// The CLI sign-in is expired or absent; hosting cannot proceed until the
    /// user re-authenticates via `devtunnel user login`.
    ReloginRequired { tunnel_id: String },
}

/// The control handle for the host engine. Implementations forward commands to
/// the background engine (real or no-op).
pub trait TunnelHost {
    /// Enqueues a command for the engine. Non-blocking.
    fn send(&self, cmd: HostCommand);
}

/// A host that drops every command. Used in the default (non-hosting) build so
/// the UI can wire the same control surface without the SDK present.
struct NoopHost;

impl TunnelHost for NoopHost {
    fn send(&self, _cmd: HostCommand) {}
}

/// A host backed by the SDK engine thread. `send` forwards each command to the
/// engine's command channel; a closed channel (engine gone) drops the command.
#[cfg(feature = "hosting")]
struct EngineHost {
    cmd_tx: Sender<HostCommand>,
}

#[cfg(feature = "hosting")]
impl TunnelHost for EngineHost {
    fn send(&self, cmd: HostCommand) {
        let _ = self.cmd_tx.send(cmd);
    }
}

/// Spawns the host engine and returns its control handle.
///
/// With `--features hosting` this starts the SDK-backed engine thread and returns
/// a handle that forwards commands to it. Without the feature it returns a
/// [`NoopHost`].
#[cfg(feature = "hosting")]
pub fn spawn(events: Sender<HostEvent>) -> Box<dyn TunnelHost> {
    let cmd_tx = engine::start(events);
    Box::new(EngineHost { cmd_tx })
}

/// Spawns the host engine and returns its control handle (non-hosting build:
/// always a [`NoopHost`] that drops commands).
#[cfg(not(feature = "hosting"))]
pub fn spawn(_events: Sender<HostEvent>) -> Box<dyn TunnelHost> {
    Box::new(NoopHost)
}
