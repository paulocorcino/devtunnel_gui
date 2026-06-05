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

// The `engine` submodule (`#[cfg(feature = "hosting")] mod engine;`) is added in
// Stage 2 together with its `engine.rs` file; Stage 1 keeps all logic inline.

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

/// Spawns the host engine and returns its control handle.
///
/// With `--features hosting` this returns a placeholder host (the real engine is
/// wired in Stage 2). Without the feature it returns a [`NoopHost`].
#[cfg(feature = "hosting")]
pub fn spawn(_events: Sender<HostEvent>) -> Box<dyn TunnelHost> {
    // Placeholder until Stage 2 wires `engine::start(events)`. Keeping a no-op
    // here lets the `hosting` build compile and link end-to-end in Stage 1.
    let _ = &_events;
    Box::new(NoopHost)
}

/// Spawns the host engine and returns its control handle (non-hosting build:
/// always a [`NoopHost`] that drops commands).
#[cfg(not(feature = "hosting"))]
pub fn spawn(_events: Sender<HostEvent>) -> Box<dyn TunnelHost> {
    Box::new(NoopHost)
}
