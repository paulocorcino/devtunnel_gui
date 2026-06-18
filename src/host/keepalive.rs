//! Pure keep-alive state machine for the host engine (issue #35).
//!
//! `host_group` (in `engine.rs`) used to inline every keep-alive policy decision
//! — reconnect on relay drop, exponential backoff, periodic token re-mint, and
//! the auth-error → relogin path — inside an async loop fused to the SDK's
//! `RelayTunnelHost`, leaving the most failure-prone logic in the app untested.
//!
//! This module holds that policy as a pure transition function with **zero**
//! SDK, CLI, or channel dependencies: it imports only [`std::time::Duration`].
//! The driver feeds it [`ConnEvent`]s (connection outcomes) and executes the
//! returned [`Action`]s. Because it is pure, it is unit-tested without the
//! vendored-OpenSSL toolchain — the tests run under a plain `cargo test`.

use std::time::Duration;

/// Re-mint the host/manage tokens before their ~24h expiry. 20h leaves headroom.
pub const REMINT_AFTER: Duration = Duration::from_secs(20 * 60 * 60);
/// Base backoff after a relay drop; doubles up to [`RECONNECT_BACKOFF_MAX`].
const RECONNECT_BACKOFF_START: Duration = Duration::from_secs(2);
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(60);

/// A connection outcome fed into the state machine by the driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnEvent {
    /// The relay connect (and port forwarding) succeeded.
    Connected,
    /// The live relay session dropped; the driver wants to reconnect.
    RelayDropped,
    /// The ~20h re-mint timer fired; reconnect with fresh tokens.
    RemintDue,
    /// A connect attempt failed. `auth` is true when the failure is an expired
    /// or absent CLI sign-in (retrying is pointless until the user re-auths).
    ConnectFailed { auth: bool },
}

/// What the driver should execute next, returned by [`KeepAliveState::next`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Hold the live connection and wait for the next outcome (no sleep).
    Await,
    /// Sleep for the given backoff, then (re)connect.
    Sleep(Duration),
    /// The sign-in is expired: emit `ReloginRequired`, surface an error, stop.
    Relogin,
}

/// Presentation phase. The driver maps it to `HostState::Connecting` (first
/// attempt) vs. `HostState::Reconnecting` (every attempt after the first).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// No successful connect or failed attempt yet — show "Connecting".
    Initial,
    /// At least one attempt has happened — show "Reconnecting".
    Reconnect,
}

/// The keep-alive policy state: the current reconnect backoff and whether this
/// is still the first connection attempt. Pure — no SDK/CLI/channel state.
pub struct KeepAliveState {
    backoff: Duration,
    first_attempt: bool,
}

impl KeepAliveState {
    /// A fresh state: backoff at [`RECONNECT_BACKOFF_START`], first attempt.
    pub fn new() -> Self {
        Self {
            backoff: RECONNECT_BACKOFF_START,
            first_attempt: true,
        }
    }

    /// Whether no attempt has completed yet (drives Connecting vs Reconnecting).
    pub fn first_attempt(&self) -> bool {
        self.first_attempt
    }

    /// The presentation phase for the next attempt.
    pub fn phase(&self) -> Phase {
        if self.first_attempt {
            Phase::Initial
        } else {
            Phase::Reconnect
        }
    }

    /// Advances the state machine for one connection outcome and returns the
    /// action the driver must execute. Mirrors the original `host_group`
    /// control flow exactly (asymmetric backoff reset: a success resets the
    /// backoff, consecutive connect-failures keep doubling it).
    pub fn next(&mut self, event: ConnEvent) -> Action {
        match event {
            // Success: reset the backoff and leave the first-attempt phase.
            ConnEvent::Connected => {
                self.backoff = RECONNECT_BACKOFF_START;
                self.first_attempt = false;
                Action::Await
            }
            // A live session ended (drop or re-mint): sleep the current backoff,
            // then double it (capped) for the next attempt.
            ConnEvent::RelayDropped | ConnEvent::RemintDue => Action::Sleep(self.bump()),
            // Expired sign-in: stop and ask the user to re-authenticate.
            ConnEvent::ConnectFailed { auth: true } => Action::Relogin,
            // Recoverable connect failure: leave the first-attempt phase and
            // back off without resetting (consecutive failures keep doubling).
            ConnEvent::ConnectFailed { auth: false } => {
                self.first_attempt = false;
                Action::Sleep(self.bump())
            }
        }
    }

    /// Returns the current backoff and then doubles it, capped at
    /// [`RECONNECT_BACKOFF_MAX`].
    fn bump(&mut self) -> Duration {
        let current = self.backoff;
        self.backoff = (self.backoff * 2).min(RECONNECT_BACKOFF_MAX);
        current
    }
}

impl Default for KeepAliveState {
    fn default() -> Self {
        Self::new()
    }
}
